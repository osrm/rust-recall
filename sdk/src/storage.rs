// Copyright 2024 ADM Contributors
// SPDX-License-Identifier: Apache-2.0, MIT

use anyhow::anyhow;
use fendermint_actor_blobs::GetAccountParams;
use fendermint_actor_blobs::Method::{GetAccount, GetStats};
use fendermint_vm_actor_interface::blobs::BLOBS_ACTOR_ADDR;
use fendermint_vm_message::query::FvmQueryHeight;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use serde::{Deserialize, Serialize};
use tendermint::abci::response::DeliverTx;

use adm_provider::message::{local_message, GasParams};
use adm_provider::query::QueryProvider;
use adm_provider::response::decode_bytes;
use adm_provider::tx::BroadcastMode;

// Commands to support:
//   ✓ adm storage stats (subnet-wide summary)
//   ✓ adm storage usage --address (see usage by account)
//   adm storage add (add a blob directly)
//   adm storage get [hash] (get a blob info directly)
//   adm storage cat [hash] (get a blob directly)
//   adm storage ls --address (list blobs by account)

/// Options for funding an account.
#[derive(Clone, Default, Debug)]
pub struct FundOptions {
    /// Broadcast mode for the transaction.
    pub broadcast_mode: BroadcastMode,
    /// Gas params for the transaction.
    pub gas_params: GasParams,
}

/// Storage usage stats for an account.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    // Total size of all blobs managed by the account.
    pub capacity_used: String,
}

impl From<fendermint_actor_blobs::Account> for Usage {
    fn from(v: fendermint_actor_blobs::Account) -> Self {
        Self {
            capacity_used: v.capacity_used.to_string(),
        }
    }
}

/// Subnet-wide storage statistics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageStats {
    /// The total free storage capacity of the subnet.
    pub capacity_free: String,
    /// The total used storage capacity of the subnet.
    pub capacity_used: String,
    /// Total number of actively stored blobs.
    pub num_blobs: u64,
    /// Total number of currently resolving blobs.
    pub num_resolving: u64,
}

impl From<fendermint_actor_blobs::GetStatsReturn> for StorageStats {
    fn from(v: fendermint_actor_blobs::GetStatsReturn) -> Self {
        Self {
            capacity_free: v.capacity_free.to_string(),
            capacity_used: v.capacity_used.to_string(),
            num_blobs: v.num_blobs,
            num_resolving: v.num_resolving,
        }
    }
}

/// A static wrapper around ADM storage methods.
pub struct Storage {}

impl Storage {
    pub async fn stats(
        provider: &impl QueryProvider,
        height: FvmQueryHeight,
    ) -> anyhow::Result<StorageStats> {
        let message = local_message(BLOBS_ACTOR_ADDR, GetStats as u64, Default::default());
        let response = provider.call(message, height, decode_stats).await?;
        Ok(response.value)
    }

    pub async fn usage(
        provider: &impl QueryProvider,
        address: Address,
        height: FvmQueryHeight,
    ) -> anyhow::Result<Usage> {
        let params = GetAccountParams(address);
        let params = RawBytes::serialize(params)?;
        let message = local_message(BLOBS_ACTOR_ADDR, GetAccount as u64, params);
        let response = provider.call(message, height, decode_usage).await?;
        if let Some(account) = response.value {
            Ok(account)
        } else {
            Ok(Usage::default())
        }
    }
}

fn decode_stats(deliver_tx: &DeliverTx) -> anyhow::Result<StorageStats> {
    let data = decode_bytes(deliver_tx)?;
    fvm_ipld_encoding::from_slice::<fendermint_actor_blobs::GetStatsReturn>(&data)
        .map(|v| v.into())
        .map_err(|e| anyhow!("error parsing as StorageStats: {e}"))
}

fn decode_usage(deliver_tx: &DeliverTx) -> anyhow::Result<Option<Usage>> {
    let data = decode_bytes(deliver_tx)?;
    fvm_ipld_encoding::from_slice::<Option<fendermint_actor_blobs::Account>>(&data)
        .map(|v| v.map(|v| v.into()))
        .map_err(|e| anyhow!("error parsing as Option<Usage>: {e}"))
}
