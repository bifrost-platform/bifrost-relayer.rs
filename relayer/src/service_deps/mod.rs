mod btc_deps;
mod full_deps;
mod handler_deps;
mod manager_deps;
mod periodic_deps;
mod substrate_deps;

pub use btc_deps::BtcDeps;
pub use full_deps::FullDeps;
pub use handler_deps::HandlerDeps;
pub use manager_deps::ManagerDeps;
pub use periodic_deps::PeriodicDeps;
pub use substrate_deps::SubstrateDeps;

use alloy::{
	network::AnyNetwork,
	primitives::ChainId,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	transports::Transport,
};
use bitcoincore_rpc::{Auth, Client as BitcoinClient};
use br_client::{
	btc::{
		block::BlockManager,
		handlers::{InboundHandler, OutboundHandler},
		storage::keypair::KeypairStorage,
	},
	eth::{
		events::EventManager,
		handlers::{RoundupRelayHandler, SocketRelayHandler},
		ClientMap, EthClient,
	},
	substrate::tx::UnsignedTransactionManager,
};
use br_periodic::{
	BitcoinRollbackVerifier, HeartbeatSender, KeypairMigrator, OraclePriceFeeder, PsbtSigner,
	PubKeyPreSubmitter, PubKeySubmitter, RoundupEmitter, SocketRollbackEmitter,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, HandlerType},
	constants::{
		cli::DEFAULT_BITCOIN_BLOCK_CONFIRMATIONS,
		errors::{
			INVALID_BIFROST_NATIVENESS, INVALID_BITCOIN_NETWORK, INVALID_CHAIN_ID,
			INVALID_PROVIDER_URL,
		},
	},
	contracts::socket::Socket_Struct::Socket_Message,
	substrate::MigrationSequence,
	tx::XtRequestSender,
};
use miniscript::bitcoin::Network;
use sc_service::TaskManager;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{mpsc::UnboundedSender, RwLock};
