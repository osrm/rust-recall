// Copyright 2024 ADM Contributors
// Copyright 2022-2024 Protocol Labs
// SPDX-License-Identifier: Apache-2.0, MIT

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use ethers::{
    core::k256::ecdsa::SigningKey,
    middleware::{Middleware, SignerMiddleware},
    prelude::{
        Authorization, Http, LocalWallet, Provider, Signer as EthSigner, Wallet, I256, U256,
    },
    types::TransactionReceipt,
};
use ethers_contract::ContractCall;
use fvm_shared::{address::Address, econ::TokenAmount};
use gateway_manager_facet::{FvmAddress, GatewayManagerFacet, SubnetID};
use ipc_actors_abis::gateway_manager_facet;
use ipc_api::evm::payload_to_evm_address;
use ipc_provider::config::{subnet::SubnetConfig, Subnet};
use num_traits::ToPrimitive;
use reqwest::{header::HeaderValue, Client};

use adm_signer::Signer;

pub type DefaultSignerMiddleware = SignerMiddleware<Provider<Http>, Wallet<SigningKey>>;

/// Default polling time used by the Ethers provider to check for pending
/// transactions and events. Default is 7, and for our child subnets we
/// can reduce it to the block time (or potentially less)
const ETH_PROVIDER_POLLING_TIME: Duration = Duration::from_secs(1);
/// Maximum number of retries to fetch a transaction receipt.
/// The number of retries should ensure that for the block time
/// of the network the number of retires considering the polling
/// time above waits enough tie to get the transaction receipt.
/// We currently support a low polling time and high number of
/// retries so these numbers accommodate fast subnets with slow
/// roots (like Calibration and mainnet).
const TRANSACTION_RECEIPT_RETRIES: usize = 200;

fn get_eth_signer(
    signer: &impl Signer,
    subnet: &Subnet,
) -> anyhow::Result<DefaultSignerMiddleware> {
    let url = subnet.rpc_http().clone();
    let auth_token = subnet.auth_token();

    let mut client = Client::builder();
    if let Some(auth_token) = auth_token {
        let auth = Authorization::Bearer(auth_token);
        let mut auth_value = HeaderValue::from_str(&auth.to_string())?;
        auth_value.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, auth_value);
        client = client.default_headers(headers);
    }
    if let Some(timeout) = subnet.rpc_timeout() {
        client = client.timeout(timeout);
    }
    let client = client.build()?;

    let provider = Http::new_with_client(url, client);
    let mut provider = Provider::new(provider);
    // set polling interval for provider to fit fast child subnets block times.
    // TODO: We may want to make it dynamic so it adjusts depending on the type of network
    // so we don't have a too slow or too fast polling for the underlying block times.
    provider.set_interval(ETH_PROVIDER_POLLING_TIME);

    let sk = match signer.secret_key() {
        Some(sk) => sk.serialize(),
        None => return Err(anyhow!("signer does not expose secret key")),
    };
    let wallet = LocalWallet::from_bytes(sk.as_slice())?.with_chain_id(signer.chain_id());

    Ok(SignerMiddleware::new(provider, wallet))
}

pub struct SubnetManager {
    subnet_id: SubnetID,
    gateway: Box<GatewayManagerFacet<DefaultSignerMiddleware>>,
}

impl SubnetManager {
    pub fn new(signer: &impl Signer, subnet: Subnet) -> anyhow::Result<Self> {
        let subnet_id = gateway_manager_facet::SubnetID::try_from(&subnet.id)?;
        let signer = get_eth_signer(signer, &subnet)?;
        let SubnetConfig::Fevm(config) = &subnet.config;
        let address = payload_to_evm_address(config.gateway_addr.payload())?;
        let gateway = GatewayManagerFacet::new(address, Arc::new(signer));
        Ok(Self {
            subnet_id,
            gateway: Box::new(gateway),
        })
    }

    pub async fn deposit(
        &self,
        to: Address,
        amount: TokenAmount,
    ) -> anyhow::Result<TransactionReceipt> {
        let value = amount
            .atto()
            .to_u128()
            .ok_or_else(|| anyhow!("invalid value to fund"))?;

        let mut call = self
            .gateway
            .fund(self.subnet_id.clone(), FvmAddress::try_from(to)?);
        call.tx.set_value(value);

        self.send(call).await
    }

    pub async fn withdraw(
        &self,
        to: Address,
        amount: TokenAmount,
    ) -> anyhow::Result<TransactionReceipt> {
        let value = amount
            .atto()
            .to_u128()
            .ok_or_else(|| anyhow!("invalid value to fund"))?;

        let mut call = self.gateway.release(FvmAddress::try_from(to)?);
        call.tx.set_value(value);

        self.send(call).await
    }

    async fn send(
        &self,
        call: ContractCall<DefaultSignerMiddleware, ()>,
    ) -> anyhow::Result<TransactionReceipt> {
        let call = call_with_premium_estimation(self.gateway.client(), call).await?;
        let tx = call.send().await?;
        match tx.retries(TRANSACTION_RECEIPT_RETRIES).await? {
            Some(receipt) => Ok(receipt),
            None => Err(anyhow!(
                "txn sent to network, but receipt cannot be obtained, please check scanner"
            )),
        }
    }
}

/// Receives an input `FunctionCall` and returns a new instance
/// after estimating an optimal `gas_premium` for the transaction
pub(crate) async fn call_with_premium_estimation<B, D, M>(
    signer: Arc<DefaultSignerMiddleware>,
    call: ethers_contract::FunctionCall<B, D, M>,
) -> anyhow::Result<ethers_contract::FunctionCall<B, D, M>>
where
    B: std::borrow::Borrow<D>,
    M: ethers::abi::Detokenize,
{
    let (max_priority_fee_per_gas, _) = premium_estimation(signer).await?;
    Ok(call.gas_price(max_priority_fee_per_gas))
}

/// Returns an estimation of an optimal `gas_premium` and `gas_fee_cap`
/// for a transaction considering the average premium, base_fee and reward percentile from
/// past blocks
/// This is adaptation of ethers' `eip1559_default_estimator`:
/// https://github.com/gakonst/ethers-rs/blob/5dcd3b7e754174448f9a8cbfc0523896609629f9/ethers-core/src/utils/mod.rs#L476
async fn premium_estimation(signer: Arc<DefaultSignerMiddleware>) -> anyhow::Result<(U256, U256)> {
    let base_fee_per_gas = signer
        .get_block(ethers::types::BlockNumber::Latest)
        .await?
        .ok_or_else(|| anyhow!("Latest block not found"))?
        .base_fee_per_gas
        .ok_or_else(|| anyhow!("EIP-1559 not activated"))?;

    let fee_history = signer
        .fee_history(
            ethers::utils::EIP1559_FEE_ESTIMATION_PAST_BLOCKS,
            ethers::types::BlockNumber::Latest,
            &[ethers::utils::EIP1559_FEE_ESTIMATION_REWARD_PERCENTILE],
        )
        .await?;

    let max_priority_fee_per_gas = estimate_priority_fee(fee_history.reward); //overestimate?
    let potential_max_fee = base_fee_surged(base_fee_per_gas);
    let max_fee_per_gas = if max_priority_fee_per_gas > potential_max_fee {
        max_priority_fee_per_gas + potential_max_fee
    } else {
        potential_max_fee
    };

    Ok((max_priority_fee_per_gas, max_fee_per_gas))
}

/// Implementation borrowed from
/// https://github.com/gakonst/ethers-rs/blob/ethers-v2.0.8/ethers-core/src/utils/mod.rs#L582
/// Refer to the implementation for unit tests
fn base_fee_surged(base_fee_per_gas: U256) -> U256 {
    if base_fee_per_gas <= U256::from(40_000_000_000u64) {
        base_fee_per_gas * 2
    } else if base_fee_per_gas <= U256::from(100_000_000_000u64) {
        base_fee_per_gas * 16 / 10
    } else if base_fee_per_gas <= U256::from(200_000_000_000u64) {
        base_fee_per_gas * 14 / 10
    } else {
        base_fee_per_gas * 12 / 10
    }
}

/// Implementation borrowed from
/// https://github.com/gakonst/ethers-rs/blob/ethers-v2.0.8/ethers-core/src/utils/mod.rs#L536
/// Refer to the implementation for unit tests
fn estimate_priority_fee(rewards: Vec<Vec<U256>>) -> U256 {
    let mut rewards: Vec<U256> = rewards
        .iter()
        .map(|r| r[0])
        .filter(|r| *r > U256::zero())
        .collect();
    if rewards.is_empty() {
        return U256::zero();
    }
    if rewards.len() == 1 {
        return rewards[0];
    }
    // Sort the rewards as we will eventually take the median.
    rewards.sort();

    // A copy of the same vector is created for convenience to calculate percentage change
    // between subsequent fee values.
    let mut rewards_copy = rewards.clone();
    rewards_copy.rotate_left(1);

    let mut percentage_change: Vec<I256> = rewards
        .iter()
        .zip(rewards_copy.iter())
        .map(|(a, b)| {
            let a = I256::try_from(*a).expect("priority fee overflow");
            let b = I256::try_from(*b).expect("priority fee overflow");
            ((b - a) * 100) / a
        })
        .collect();
    percentage_change.pop();

    // Fetch the max of the percentage change, and that element's index.
    let max_change = percentage_change.iter().max().unwrap();
    let max_change_index = percentage_change
        .iter()
        .position(|&c| c == *max_change)
        .unwrap();

    // If we encountered a big change in fees at a certain position, then consider only
    // the values >= it.
    let values = if *max_change >= ethers::utils::EIP1559_FEE_ESTIMATION_THRESHOLD_MAX_CHANGE.into()
        && (max_change_index >= (rewards.len() / 2))
    {
        rewards[max_change_index..].to_vec()
    } else {
        rewards
    };

    // Return the median.
    values[values.len() / 2]
}
