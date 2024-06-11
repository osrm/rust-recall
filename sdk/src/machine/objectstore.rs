// Copyright 2024 ADM Contributors
// SPDX-License-Identifier: Apache-2.0, MIT

use std::cmp::min;

use anyhow::anyhow;
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine};
use bytes::Bytes;
use fendermint_actor_machine::WriteAccess;
use fendermint_actor_objectstore::{
    DeleteParams, GetParams,
    Method::{DeleteObject, GetObject, ListObjects, PutObject},
    Object, ObjectKind, ObjectList, PutParams,
};
use fendermint_vm_actor_interface::adm::Kind;
use fendermint_vm_message::{query::FvmQueryHeight, signed::Object as MessageObject};
use fvm_ipld_encoding::{serde_bytes::ByteBuf, RawBytes};
use fvm_shared::address::Address;
use indicatif::HumanDuration;
use num_traits::Zero;
use tendermint::abci::response::DeliverTx;
use tendermint_rpc::Client;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    time::Instant,
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use unixfs_v1::file::adder::{Chunker, FileAdder};

use adm_provider::{
    message::{local_message, object_upload_message, GasParams},
    object::ObjectProvider,
    query::QueryProvider,
    response::{decode_bytes, decode_cid, Cid},
    tx::{BroadcastMode, TxReceipt},
    Provider,
};
use adm_signer::Signer;

use crate::progress::{new_message_bar, new_multi_bar, SPARKLE};
use crate::{
    machine::{deploy_machine, DeployTxReceipt, Machine},
    progress::new_progress_bar,
};

const MAX_INTERNAL_OBJECT_LENGTH: usize = 1024;

/// Object add options.
#[derive(Clone, Default, Debug)]
pub struct AddOptions {
    /// Overwrite the object if it already exists.
    pub overwrite: bool,
    /// Broadcast mode for the transaction.
    pub broadcast_mode: BroadcastMode,
    /// Gas params for the transaction.
    pub gas_params: GasParams,
    /// Whether to show progress-related output (useful for command-line interfaces).
    pub show_progress: bool,
}

/// Object delete options.
#[derive(Clone, Default, Debug)]
pub struct DeleteOptions {
    /// Broadcast mode for the transaction.
    pub broadcast_mode: BroadcastMode,
    /// Gas params for the transaction.
    pub gas_params: GasParams,
}

/// Object get options.
#[derive(Clone, Default, Debug)]
pub struct GetOptions {
    /// Optional range of bytes to get from the object.
    /// Format: "start-end" (inclusive).
    /// Example: "0-99" (first 100 bytes).
    /// This follows the HTTP range header format:
    /// `<https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Range>`
    pub range: Option<String>,
    /// Query block height.
    pub height: FvmQueryHeight,
    /// Whether to show progress-related output (useful for command-line interfaces).
    pub show_progress: bool,
}

/// Object query options.
#[derive(Clone, Debug)]
pub struct QueryOptions {
    /// The prefix to filter objects by.
    pub prefix: String,
    /// The delimiter used to define object hierarchy.
    pub delimiter: String,
    /// The offset to start listing objects from.
    pub offset: u64,
    /// The maximum number of objects to list.
    pub limit: u64,
    /// Query block height.
    pub height: FvmQueryHeight,
}

impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
            prefix: Default::default(),
            delimiter: "/".into(),
            offset: Default::default(),
            limit: Default::default(),
            height: Default::default(),
        }
    }
}

/// Parse a range string and return start and end byte positions.
fn parse_range(range: String, size: u64) -> anyhow::Result<(u64, u64)> {
    let range: Vec<String> = range.split('-').map(|n| n.to_string()).collect();
    if range.len() != 2 {
        return Err(anyhow!("invalid range format"));
    }
    let (start, end): (u64, u64) = match (!range[0].is_empty(), !range[1].is_empty()) {
        (true, true) => (range[0].parse::<u64>()?, range[1].parse::<u64>()?),
        (true, false) => (range[0].parse::<u64>()?, size - 1),
        (false, true) => {
            let last = range[1].parse::<u64>()?;
            if last > size {
                (0, size - 1)
            } else {
                (size - last, size - 1)
            }
        }
        (false, false) => (0, size - 1),
    };
    if start > end || end >= size {
        return Err(anyhow!("invalid range"));
    }
    Ok((start, end))
}

/// A machine for S3-like object storage.
pub struct ObjectStore {
    address: Address,
}

#[async_trait]
impl Machine for ObjectStore {
    const KIND: Kind = Kind::ObjectStore;

    async fn new<C>(
        provider: &impl Provider<C>,
        signer: &mut impl Signer,
        write_access: WriteAccess,
        gas_params: GasParams,
    ) -> anyhow::Result<(Self, DeployTxReceipt)>
    where
        C: Client + Send + Sync,
    {
        let (address, tx) = deploy_machine(
            provider,
            signer,
            Kind::ObjectStore,
            write_access,
            gas_params,
        )
        .await?;
        Ok((Self::attach(address), tx))
    }

    fn attach(address: Address) -> Self {
        ObjectStore { address }
    }

    fn address(&self) -> Address {
        self.address
    }
}

impl ObjectStore {
    /// Add an object into the object store.
    pub async fn add<C, R>(
        &self,
        provider: &impl Provider<C>,
        signer: &mut impl Signer,
        key: &str,
        mut reader: R,
        options: AddOptions,
    ) -> anyhow::Result<TxReceipt<Cid>>
    where
        C: Client + Send + Sync,
        R: AsyncRead + AsyncSeek + Unpin + Send + 'static,
    {
        let started = Instant::now();
        let bars = new_multi_bar(!options.show_progress);
        let msg_bar = bars.add(new_message_bar());

        // Sample reader to determine what kind of object will be added
        let mut sample = vec![0; MAX_INTERNAL_OBJECT_LENGTH + 1];
        let sampled = reader.read(&mut sample).await?;
        if sampled.is_zero() {
            return Err(anyhow!("cannot add empty object"));
        }
        reader.rewind().await?;

        let tx = if sampled > MAX_INTERNAL_OBJECT_LENGTH {
            // Handle as a detached object

            // Generate object Cid
            // We do this here to avoid moving the reader
            let chunk_size = 1024 * 1024; // size-1048576
            let mut adder = FileAdder::builder()
                .with_chunker(Chunker::Size(chunk_size))
                .build();
            let mut buffer = vec![0; chunk_size];
            let mut reader_size: usize = 0;
            let mut object_size: usize = 0;

            msg_bar.set_prefix("[1/3]");
            let mut chunk = Cid::from(cid::Cid::default());
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => {
                        break;
                    }
                    Ok(n) => {
                        reader_size += n;
                        let (leaf, n) = adder.push(&buffer[..n]);
                        for (c, _) in leaf {
                            chunk = Cid::from(cid::Cid::try_from(c.to_bytes())?);
                            msg_bar.set_message(format!("Processed chunk: {}", c));
                        }
                        object_size += n;
                    }
                    Err(e) => {
                        return Err(e.into());
                    }
                }
            }
            let unixfs_iterator = adder.finish();
            // Turns out if input is equal to chunk size, the iterator will be empty,
            // and the object cid will be equal to the first processed chunk ¯\_(ツ)_/¯
            let last = unixfs_iterator.last();
            let object_cid = match last {
                Some((c, _)) => Cid::from(cid::Cid::try_from(c.to_bytes())?),
                None => chunk,
            };

            // Rewind and stream for uploading
            msg_bar.set_prefix("[2/3]");
            msg_bar.set_message(format!("Uploading {} to network...", object_cid));
            let pro_bar = bars.add(new_progress_bar(reader_size));
            reader.rewind().await?;
            let mut stream = ReaderStream::new(reader);
            let async_stream = async_stream::stream! {
                let mut progress: usize = 0;
                while let Some(chunk) = stream.next().await {
                    if let Ok(chunk) = &chunk {
                        progress = min(progress + chunk.len(), reader_size);
                        pro_bar.set_position(progress as u64);
                    }
                    yield chunk;
                }
                pro_bar.finish_and_clear();
            };

            // Upload Object to Object API
            let response_cid = self
                .upload(
                    provider,
                    signer,
                    key,
                    async_stream,
                    object_cid,
                    object_size,
                    options.overwrite,
                )
                .await?;

            // Verify uploaded CID with locally computed CID
            if response_cid != object_cid {
                return Err(anyhow!("cannot verify object; cid does not match remote"));
            }

            // Broadcast transaction with Object's CID
            msg_bar.set_prefix("[3/3]");
            msg_bar.set_message("Broadcasting transaction...");
            let params = PutParams {
                key: key.into(),
                kind: ObjectKind::External(object_cid.0),
                overwrite: options.overwrite,
            };
            let serialized_params = RawBytes::serialize(params.clone())?;
            let object = Some(MessageObject::new(
                params.key.clone(),
                object_cid.0,
                self.address,
            ));
            let message = signer
                .transaction(
                    self.address,
                    Default::default(),
                    PutObject as u64,
                    serialized_params,
                    object,
                    options.gas_params,
                )
                .await?;

            let tx = provider
                .perform(message, options.broadcast_mode, decode_cid)
                .await?;

            msg_bar.println(format!(
                "{} Added detached object in {} (cid={}; size={})",
                SPARKLE,
                HumanDuration(started.elapsed()),
                object_cid,
                object_size
            ));
            tx
        } else {
            // Handle as an internal object

            // Broadcast transaction with Object's CID
            msg_bar.set_prefix("[1/1]");
            msg_bar.set_message("Broadcasting transaction...");
            sample.truncate(sampled);
            let params = PutParams {
                key: key.into(),
                kind: ObjectKind::Internal(ByteBuf(sample)),
                overwrite: options.overwrite,
            };
            let serialized_params = RawBytes::serialize(params)?;
            let message = signer
                .transaction(
                    self.address,
                    Default::default(),
                    PutObject as u64,
                    serialized_params,
                    None,
                    options.gas_params,
                )
                .await?;
            let tx = provider
                .perform(message, options.broadcast_mode, decode_cid)
                .await?;

            msg_bar.println(format!(
                "{} Added object in {} (size={})",
                SPARKLE,
                HumanDuration(started.elapsed()),
                sampled
            ));
            tx
        };

        msg_bar.finish_and_clear();
        Ok(tx)
    }

    /// Uploads an object to the Object API for staging.
    #[allow(clippy::too_many_arguments)]
    async fn upload<S>(
        &self,
        provider: &impl ObjectProvider,
        signer: &mut impl Signer,
        key: &str,
        stream: S,
        cid: Cid,
        size: usize,
        overwrite: bool,
    ) -> anyhow::Result<Cid>
    where
        S: futures_core::stream::TryStream + Send + 'static,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        Bytes: From<S::Ok>,
    {
        let from = signer.address();
        let params = PutParams {
            key: key.into(),
            kind: ObjectKind::External(cid.0),
            overwrite,
        };
        let serialized_params = RawBytes::serialize(params)?;

        let message =
            object_upload_message(from, self.address, PutObject as u64, serialized_params);
        let singed_message = signer.sign_message(
            message,
            Some(MessageObject::new(key.into(), cid.0, self.address)),
        )?;
        let serialized_signed_message = fvm_ipld_encoding::to_vec(&singed_message)?;

        let chain_id = match signer.subnet_id() {
            Some(id) => id.chain_id(),
            None => {
                return Err(anyhow!("failed to get subnet ID from signer"));
            }
        };

        let body = reqwest::Body::wrap_stream(stream);
        let response = provider
            .upload(
                body,
                size,
                general_purpose::URL_SAFE.encode(&serialized_signed_message),
                chain_id.into(),
            )
            .await?;

        Ok(response)
    }

    /// Delete an object.
    pub async fn delete<C>(
        &self,
        provider: &impl Provider<C>,
        signer: &mut impl Signer,
        key: &str,
        options: DeleteOptions,
    ) -> anyhow::Result<TxReceipt<Cid>>
    where
        C: Client + Send + Sync,
    {
        let params = DeleteParams { key: key.into() };
        let params = RawBytes::serialize(params)?;
        let message = signer
            .transaction(
                self.address,
                Default::default(),
                DeleteObject as u64,
                params,
                None,
                options.gas_params,
            )
            .await?;
        provider
            .perform(message, options.broadcast_mode, decode_cid)
            .await
    }

    /// Get an object at the given key, range, and height.
    pub async fn get<W>(
        &self,
        provider: &(impl QueryProvider + ObjectProvider),
        key: &str,
        mut writer: W,
        options: GetOptions,
    ) -> anyhow::Result<()>
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let started = Instant::now();
        let bars = new_multi_bar(!options.show_progress);
        let msg_bar = bars.add(new_message_bar());

        msg_bar.set_prefix("[1/2]");
        msg_bar.set_message("Getting object info...");
        let params = GetParams { key: key.into() };
        let params = RawBytes::serialize(params)?;
        let message = local_message(self.address, GetObject as u64, params);
        let response = provider.call(message, options.height, decode_get).await?;

        let object = response
            .value
            .ok_or_else(|| anyhow!("object not found for key '{}'", key))?;
        match object {
            Object::Internal(buf) => {
                msg_bar.set_prefix("[2/2]");
                msg_bar.set_message(format!("Writing {} bytes...", buf.0.len()));
                if let Some(range) = options.range {
                    let (start, end) = parse_range(range, buf.0.len() as u64)?;
                    writer
                        .write_all(&buf.0[start as usize..=end as usize])
                        .await?;
                } else {
                    writer.write_all(&buf.0).await?;
                }

                msg_bar.println(format!(
                    "{} Got object from chain in {} (size={})",
                    SPARKLE,
                    HumanDuration(started.elapsed()),
                    buf.0.len()
                ));
            }
            Object::External((buf, resolved)) => {
                let cid = cid::Cid::try_from(buf.0)?;
                if !resolved {
                    return Err(anyhow!("object is not resolved"));
                }
                msg_bar.set_prefix("[2/2]");
                msg_bar.set_message(format!("Downloading {}... ", cid));

                let object_size = provider
                    .size(self.address, key, options.height.into())
                    .await?;
                let pro_bar = bars.add(new_progress_bar(object_size));
                let response = provider
                    .download(self.address, key, options.range, options.height.into())
                    .await?;
                let mut stream = response.bytes_stream();
                let mut progress = 0;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(chunk) => {
                            writer.write_all(&chunk).await?;
                            progress = min(progress + chunk.len(), object_size);
                            pro_bar.set_position(progress as u64);
                        }
                        Err(e) => {
                            return Err(anyhow!(e));
                        }
                    }
                }
                pro_bar.finish_and_clear();
                msg_bar.println(format!(
                    "{} Downloaded detached object in {} (cid={})",
                    SPARKLE,
                    HumanDuration(started.elapsed()),
                    cid
                ));
            }
        }

        msg_bar.finish_and_clear();
        Ok(())
    }

    /// Query for objects with params at the given height.
    ///
    /// Use [`QueryOptions`] for filtering and pagination.
    pub async fn query(
        &self,
        provider: &impl QueryProvider,
        options: QueryOptions,
    ) -> anyhow::Result<ObjectList> {
        let params = fendermint_actor_objectstore::ListParams {
            prefix: options.prefix.into(),
            delimiter: options.delimiter.into(),
            offset: options.offset,
            limit: options.limit,
        };
        let params = RawBytes::serialize(params)?;
        let message = local_message(self.address, ListObjects as u64, params);
        let response = provider.call(message, options.height, decode_list).await?;
        Ok(response.value)
    }
}

fn decode_get(deliver_tx: &DeliverTx) -> anyhow::Result<Option<Object>> {
    let data = decode_bytes(deliver_tx)?;
    fvm_ipld_encoding::from_slice::<Option<Object>>(&data)
        .map_err(|e| anyhow!("error parsing as Option<Object>: {e}"))
}

fn decode_list(deliver_tx: &DeliverTx) -> anyhow::Result<ObjectList> {
    let data = decode_bytes(deliver_tx)?;
    fvm_ipld_encoding::from_slice::<ObjectList>(&data)
        .map_err(|e| anyhow!("error parsing as ObjectList: {e}"))
}
