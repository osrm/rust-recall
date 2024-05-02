// Copyright 2024 ADM Contributors
// SPDX-License-Identifier: Apache-2.0, MIT

use anyhow::anyhow;
use ethers::types::TransactionReceipt;
use fendermint_actor_machine::GET_METADATA_METHOD;
use fendermint_vm_actor_interface::adm::{
    ListMetadataParams, Metadata, Method::ListMetadata, ADM_ACTOR_ADDR,
};
use fendermint_vm_message::query::FvmQueryHeight;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount, METHOD_SEND};
use ipc_provider::config::Subnet;
use tendermint::abci::response::DeliverTx;
use tendermint_rpc::Client;

use adm_provider::{
    message::{local_message, GasParams},
    response::decode_bytes,
    BroadcastMode, Provider, Tx,
};
use adm_signer::Signer;

use crate::ipc::manager::SubnetManager;

mod ipc;
pub mod machine;
pub mod network;

/// Arguments common to transactions.
#[derive(Clone, Default, Debug)]
pub struct TxArgs {
    /// Sender account sequence (nonce).
    pub sequence: Option<u64>,
    /// Gas params.
    pub gas_params: GasParams,
}

pub enum TxRecipient {
    Address(Address),
    Signer,
}

pub struct Adm {}

impl Adm {
    pub async fn list_machine_metadata<C>(
        provider: &impl Provider<C>,
        owner: Address,
        height: FvmQueryHeight,
    ) -> anyhow::Result<Vec<Metadata>>
    where
        C: Client + Send + Sync,
    {
        let input = ListMetadataParams { owner };
        let params = RawBytes::serialize(input)?;
        let message = local_message(ADM_ACTOR_ADDR, ListMetadata as u64, params);
        let response = provider.call(message, height, decode_list).await?;
        Ok(response.value)
    }

    pub async fn get_machine_metadata<C>(
        provider: &impl Provider<C>,
        address: Address,
        height: FvmQueryHeight,
    ) -> anyhow::Result<fendermint_actor_machine::Metadata>
    where
        C: Client + Send + Sync,
    {
        let message = local_message(address, GET_METADATA_METHOD, Default::default());
        let response = provider.call(message, height, decode_metadata).await?;
        Ok(response.value)
    }

    pub async fn deposit(
        signer: &impl Signer,
        to: TxRecipient,
        subnet: Subnet,
        amount: TokenAmount,
    ) -> anyhow::Result<TransactionReceipt> {
        let manager = SubnetManager::new(signer, subnet)?;
        let to = match to {
            TxRecipient::Address(addr) => addr,
            TxRecipient::Signer => signer.address(),
        };
        manager.deposit(to, amount).await
    }

    pub async fn withdraw(
        signer: &impl Signer,
        to: TxRecipient,
        subnet: Subnet,
        amount: TokenAmount,
    ) -> anyhow::Result<TransactionReceipt> {
        let manager = SubnetManager::new(signer, subnet)?;
        let to = match to {
            TxRecipient::Address(addr) => addr,
            TxRecipient::Signer => signer.address(),
        };
        manager.withdraw(to, amount).await
    }

    pub async fn transfer<C>(
        provider: &impl Provider<C>,
        signer: &mut impl Signer,
        to: Address,
        value: TokenAmount,
        args: TxArgs,
    ) -> anyhow::Result<Tx<()>>
    where
        C: Client + Send + Sync,
    {
        let message = signer.transaction(
            to,
            value,
            METHOD_SEND,
            RawBytes::default(),
            None,
            args.gas_params,
        )?;
        provider
            .perform(message, BroadcastMode::Commit, |_| Ok(()))
            .await
    }
}

fn decode_metadata(deliver_tx: &DeliverTx) -> anyhow::Result<fendermint_actor_machine::Metadata> {
    let data = decode_bytes(deliver_tx)?;
    fvm_ipld_encoding::from_slice::<fendermint_actor_machine::Metadata>(&data)
        .map_err(|e| anyhow!("error parsing as Metadata: {e}"))
}

fn decode_list(deliver_tx: &DeliverTx) -> anyhow::Result<Vec<Metadata>> {
    let data = decode_bytes(deliver_tx)?;
    fvm_ipld_encoding::from_slice::<Vec<Metadata>>(&data)
        .map_err(|e| anyhow!("error parsing as Vec<adm::Metadata>: {e}"))
}
