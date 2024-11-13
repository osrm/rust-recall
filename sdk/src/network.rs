// Copyright 2024 Hoku Contributors
// SPDX-License-Identifier: Apache-2.0, MIT

use std::fmt::Display;
use std::str::FromStr;
use std::time::Duration;

use anyhow::anyhow;
use fvm_shared::address::{set_current_network, Address, Error, Network as FvmNetwork};
use serde::{Deserialize, Deserializer};
use tendermint_rpc::Url;

use hoku_provider::util::parse_address;
use hoku_signer::SubnetID;

use crate::ipc::subnet::EVMSubnet;

const TESTNET_SUBNET_ID: &str = "/r314159/t410fvamrbjioufgzoyojg2x3nwdo26t6xucxoxl47yq"; // chain ID: 2938118273996536
const LOCALNET_SUBNET_ID: &str = "/r31337/t410fkzrz3mlkyufisiuae3scumllgalzuu3wxlxa2ly"; // chain ID: 4362550583360910
const DEVNET_SUBNET_ID: &str = "test";
const IGNITION_SUBNET_ID: &str = "/r314159/t410f2x3jiwcg6ju4bvy2lpdzc6xjo5okoktdm63mwni"; // chain ID: 2009180146406958

const TESTNET_RPC_URL: &str = "https://api.n1.hoku.sh";
const LOCALNET_RPC_URL: &str = "http://127.0.0.1:26657";
const IGNITION_RPC_URL: &str = "https://api-ignition-0.hoku.sh";

const RPC_TIMEOUT: Duration = Duration::from_secs(60);

const TESTNET_EVM_RPC_URL: &str = "https://evm-api.n1.hoku.sh";
const LOCALNET_EVM_RPC_URL: &str = "http://127.0.0.1:8645";
const IGNITION_EVM_RPC_URL: &str = "https://evm-ignition-0.hoku.sh";
const DEVNET_EVM_RPC_URL: &str = "http://127.0.0.1:8545";

const TESTNET_EVM_GATEWAY_ADDRESS: &str = "0x77aa40b105843728088c0132e43fc44348881da8";
const TESTNET_EVM_REGISTRY_ADDRESS: &str = "0x74539671a1d2f1c8f200826baba665179f53a1b7";
const TESTNET_EVM_SUPPLY_SOURCE_ADDRESS: &str = "0x8e3Fd2b47e564E7D636Fa80082f286eD038BE54b";
const LOCALNET_EVM_GATEWAY_ADDRESS: &str = "0x77aa40b105843728088c0132e43fc44348881da8";
const LOCALNET_EVM_REGISTRY_ADDRESS: &str = "0x74539671a1d2f1c8f200826baba665179f53a1b7";
const LOCALNET_EVM_SUPPLY_SOURCE_ADDRESS: &str = "0xE6E340D132b5f46d1e472DebcD681B2aBc16e57E";
const IGNITION_EVM_GATEWAY_ADDRESS: &str = "0x77aa40b105843728088c0132e43fc44348881da8";
const IGNITION_EVM_REGISTRY_ADDRESS: &str = "0x74539671a1d2f1c8f200826baba665179f53a1b7";
const IGNITION_EVM_SUPPLY_SOURCE_ADDRESS: &str = "0x20d8a696091153c4d4816ba1fdefe113f71e0905";
const DEVNET_EVM_GATEWAY_ADDRESS: &str = "0x77aa40b105843728088c0132e43fc44348881da8";
const DEVNET_EVM_REGISTRY_ADDRESS: &str = "0x74539671a1d2f1c8f200826baba665179f53a1b7";

const TESTNET_PARENT_EVM_RPC_URL: &str = "https://api.calibration.node.glif.io/rpc/v1";
const TESTNET_PARENT_EVM_GATEWAY_ADDRESS: &str = "0xe17B86E7BEFC691DAEfe2086e56B86D4253f3294";
const TESTNET_PARENT_EVM_REGISTRY_ADDRESS: &str = "0xe87AFBEC26f0fdAC69e4256dC1935bEab1e0855E";
const LOCALNET_PARENT_EVM_RPC_URL: &str = "http://127.0.0.1:8545";
const LOCALNET_PARENT_EVM_GATEWAY_ADDRESS: &str = "0x9A676e781A523b5d0C0e43731313A708CB607508";
const LOCALNET_PARENT_EVM_REGISTRY_ADDRESS: &str = "0x4ed7c70F96B99c776995fB64377f0d4aB3B0e1C1";
const IGNITION_PARENT_EVM_RPC_URL: &str = "https://api.calibration.node.glif.io/rpc/v1";
const IGNITION_PARENT_EVM_GATEWAY_ADDRESS: &str = "0xF8Abf46A1114d3B44d18F2A96D850e36FC6Ee94E";
const IGNITION_PARENT_EVM_REGISTRY_ADDRESS: &str = "0x0bb143a180b61ae6b1872bbf99dBe261A2aDde40";

const TESTNET_OBJECT_API_URL: &str = "https://object-api.n1.hoku.sh";
const LOCALNET_OBJECT_API_URL: &str = "http://127.0.0.1:8001";
const IGNITION_OBJECT_API_URL: &str = "https://object-api-ignition-0.hoku.sh";

/// Options for [`EVMSubnet`] configurations.
#[derive(Debug, Clone)]
pub struct SubnetOptions {
    /// The EVM RPC provider request timeout.
    pub evm_rpc_timeout: Duration,
    /// The EVM RPC provider authorization token.
    pub evm_rpc_auth_token: Option<String>,
}

impl Default for SubnetOptions {
    fn default() -> Self {
        Self {
            evm_rpc_timeout: RPC_TIMEOUT,
            evm_rpc_auth_token: None,
        }
    }
}

/// Network presets for a subnet configuration and RPC URLs.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Network {
    /// Network presets for mainnet.
    Mainnet,
    /// Network presets for Calibration (default pre-mainnet).
    Testnet,
    /// Network presets for a local three-node network.
    Localnet,
    /// Network presets for local development.
    Devnet,
    /// Network presets for Ignition testnet.
    Ignition,
}

impl Network {
    /// Sets the current [`FvmNetwork`].
    /// Note: This _must_ be called before using the SDK.
    pub fn init(&self) -> &Self {
        match self {
            Network::Mainnet => set_current_network(FvmNetwork::Mainnet),
            Network::Testnet | Network::Localnet | Network::Devnet | Network::Ignition => {
                set_current_network(FvmNetwork::Testnet)
            }
        }
        self
    }

    /// Returns the network [`SubnetID`].
    pub fn subnet_id(&self) -> anyhow::Result<SubnetID> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(SubnetID::from_str(TESTNET_SUBNET_ID)?),
            Network::Localnet => Ok(SubnetID::from_str(LOCALNET_SUBNET_ID)?),
            Network::Devnet => Ok(SubnetID::from_str(DEVNET_SUBNET_ID)?),
            Network::Ignition => Ok(SubnetID::from_str(IGNITION_SUBNET_ID)?),
        }
    }

    /// Returns the network [`EVMSubnet`] configuration.
    pub fn subnet_config(&self, options: SubnetOptions) -> anyhow::Result<EVMSubnet> {
        Ok(EVMSubnet {
            id: self.subnet_id()?,
            provider_http: self.evm_rpc_url()?,
            provider_timeout: Some(options.evm_rpc_timeout),
            auth_token: options.evm_rpc_auth_token,
            registry_addr: self.evm_registry()?,
            gateway_addr: self.evm_gateway()?,
            supply_source: None,
        })
    }

    /// Returns whether this network type has a parent chain.
    pub fn has_parent(&self) -> bool {
        match self {
            Network::Mainnet => true,
            Network::Testnet => true,
            Network::Localnet => true,
            Network::Ignition => true,
            Network::Devnet => false,
        }
    }

    /// Returns the network [`Url`] of the CometBFT RPC API.
    pub fn rpc_url(&self) -> anyhow::Result<Url> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(Url::from_str(TESTNET_RPC_URL)?),
            Network::Localnet | Network::Devnet => Ok(Url::from_str(LOCALNET_RPC_URL)?),
            Network::Ignition => Ok(Url::from_str(IGNITION_RPC_URL)?),
        }
    }

    /// Returns the network [`Url`] of the Object API.
    pub fn object_api_url(&self) -> anyhow::Result<Url> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(Url::from_str(TESTNET_OBJECT_API_URL)?),
            Network::Localnet | Network::Devnet => Ok(Url::from_str(LOCALNET_OBJECT_API_URL)?),
            Network::Ignition => Ok(Url::from_str(IGNITION_OBJECT_API_URL)?),
        }
    }

    /// Returns the network [`reqwest::Url`] of the EVM RPC API.
    pub fn evm_rpc_url(&self) -> anyhow::Result<reqwest::Url> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(reqwest::Url::from_str(TESTNET_EVM_RPC_URL)?),
            Network::Localnet => Ok(reqwest::Url::from_str(LOCALNET_EVM_RPC_URL)?),
            Network::Devnet => Ok(reqwest::Url::from_str(DEVNET_EVM_RPC_URL)?),
            Network::Ignition => Ok(reqwest::Url::from_str(IGNITION_EVM_RPC_URL)?),
        }
    }

    /// Returns the network [`Address`] of the EVM Gateway contract.
    pub fn evm_gateway(&self) -> anyhow::Result<Address> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(parse_address(TESTNET_EVM_GATEWAY_ADDRESS)?),
            Network::Localnet => Ok(parse_address(LOCALNET_EVM_GATEWAY_ADDRESS)?),
            Network::Devnet => Ok(parse_address(DEVNET_EVM_GATEWAY_ADDRESS)?),
            Network::Ignition => Ok(parse_address(IGNITION_EVM_GATEWAY_ADDRESS)?),
        }
    }

    /// Returns the network [`Address`] of the EVM Registry contract.
    pub fn evm_registry(&self) -> anyhow::Result<Address> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(parse_address(TESTNET_EVM_REGISTRY_ADDRESS)?),
            Network::Localnet => Ok(parse_address(LOCALNET_EVM_REGISTRY_ADDRESS)?),
            Network::Devnet => Ok(parse_address(DEVNET_EVM_REGISTRY_ADDRESS)?),
            Network::Ignition => Ok(parse_address(IGNITION_EVM_REGISTRY_ADDRESS)?),
        }
    }

    /// Returns the network [`EVMSubnet`] parent configuration.
    pub fn parent_subnet_config(&self, options: SubnetOptions) -> anyhow::Result<EVMSubnet> {
        Ok(EVMSubnet {
            id: self.subnet_id()?,
            provider_http: self.parent_evm_rpc_url()?,
            provider_timeout: Some(options.evm_rpc_timeout),
            auth_token: options.evm_rpc_auth_token,
            registry_addr: self.parent_evm_registry()?,
            gateway_addr: self.parent_evm_gateway()?,
            supply_source: Some(self.parent_evm_supply_source()?),
        })
    }

    /// Returns the network [`reqwest::Url`] of the parent EVM RPC API.
    pub fn parent_evm_rpc_url(&self) -> anyhow::Result<reqwest::Url> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(reqwest::Url::from_str(TESTNET_PARENT_EVM_RPC_URL)?),
            Network::Localnet => Ok(reqwest::Url::from_str(LOCALNET_PARENT_EVM_RPC_URL)?),
            Network::Devnet => Err(anyhow!("network has no parent")),
            Network::Ignition => Ok(reqwest::Url::from_str(IGNITION_PARENT_EVM_RPC_URL)?),
        }
    }

    /// Returns the network [`Address`] of the parent EVM Gateway contract.
    pub fn parent_evm_gateway(&self) -> anyhow::Result<Address> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(parse_address(TESTNET_PARENT_EVM_GATEWAY_ADDRESS)?),
            Network::Localnet => Ok(parse_address(LOCALNET_PARENT_EVM_GATEWAY_ADDRESS)?),
            Network::Devnet => Err(anyhow!("network has no parent")),
            Network::Ignition => Ok(parse_address(IGNITION_PARENT_EVM_GATEWAY_ADDRESS)?),
        }
    }

    /// Returns the network [`Address`] of the parent EVM Registry contract.
    pub fn parent_evm_registry(&self) -> anyhow::Result<Address> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(parse_address(TESTNET_PARENT_EVM_REGISTRY_ADDRESS)?),
            Network::Localnet => Ok(parse_address(LOCALNET_PARENT_EVM_REGISTRY_ADDRESS)?),
            Network::Devnet => Err(anyhow!("network has no parent")),
            Network::Ignition => Ok(parse_address(IGNITION_PARENT_EVM_REGISTRY_ADDRESS)?),
        }
    }

    /// Returns the network [`Address`] of the EVM Supply Source contract.
    pub fn parent_evm_supply_source(&self) -> anyhow::Result<Address> {
        match self {
            Network::Mainnet => Err(anyhow!("network is pre-mainnet")),
            Network::Testnet => Ok(parse_address(TESTNET_EVM_SUPPLY_SOURCE_ADDRESS)?),
            Network::Localnet => Ok(parse_address(LOCALNET_EVM_SUPPLY_SOURCE_ADDRESS)?),
            Network::Devnet => Err(anyhow!("network has no parent")),
            Network::Ignition => Ok(parse_address(IGNITION_EVM_SUPPLY_SOURCE_ADDRESS)?),
        }
    }
}

impl FromStr for Network {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::Testnet),
            "localnet" => Ok(Network::Localnet),
            "devnet" => Ok(Network::Devnet),
            "ignition" => Ok(Network::Ignition),
            _ => Err(Error::UnknownNetwork.to_string()),
        }
    }
}

impl Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Network::Mainnet => write!(f, "mainnet"),
            Network::Testnet => write!(f, "testnet"),
            Network::Localnet => write!(f, "localnet"),
            Network::Devnet => write!(f, "devnet"),
            Network::Ignition => write!(f, "ignition"),
        }
    }
}

impl<'de> Deserialize<'de> for Network {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        Network::from_str(&s).map_err(serde::de::Error::custom)
    }
}
