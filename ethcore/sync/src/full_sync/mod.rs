pub mod blocks;
pub mod block_sync;
pub mod chain;
pub mod sync_io;
pub mod private_tx;
pub mod snapshot;
pub mod transactions_stats;


use std::time::Duration;
use std::sync::Arc;
use std::collections::{BTreeMap, HashMap};
use std::io;

use bytes::Bytes;
use super::*;
use super::common_types::*;
use devp2p::NetworkService;
use full_sync::chain::{ETH_PROTOCOL_VERSION_63, ETH_PROTOCOL_VERSION_62,
	PAR_PROTOCOL_VERSION_1, PAR_PROTOCOL_VERSION_2, PAR_PROTOCOL_VERSION_3,
PRIVATE_TRANSACTION_PACKET, SIGNED_PRIVATE_TRANSACTION_PACKET};
use full_sync::chain::{ChainSync, SyncStatus as EthSyncStatus};
use types::pruning_info::PruningInfo;
use ethereum_types::H256;
use io::{TimerToken};
use ethcore::client::{BlockChainClient, ChainNotify, ChainRoute, ChainMessageType};
use ethcore::snapshot::SnapshotService;
use ethcore::header::BlockNumber;
use light::net::{
	self as light_net, LightProtocol, Params as LightParams,
	Capabilities, Handler as LightHandler, EventContext, SampleStore,
};
use network::{NetworkProtocolHandler, NetworkContext, PeerId, ProtocolId, NonReservedPeerMode, Error, ErrorKind, ConnectionFilter};
use full_sync::sync_io::NetSyncIo;
use parking_lot::RwLock;
use full_sync::private_tx::*;
use transaction::UnverifiedTransaction;


const PEERS_TIMER: TimerToken = 0;
const SYNC_TIMER: TimerToken = 1;
const TX_TIMER: TimerToken = 2;

/// Determine warp sync status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpSync {
	/// Warp sync is enabled.
	Enabled,
	/// Warp sync is disabled.
	Disabled,
	/// Only warp sync is allowed (no regular sync) and only after given block number.
	OnlyAndAfter(BlockNumber),
}

impl WarpSync {
	/// Returns true if warp sync is enabled.
	pub fn is_enabled(&self) -> bool {
		match *self {
			WarpSync::Enabled => true,
			WarpSync::OnlyAndAfter(_) => true,
			WarpSync::Disabled => false,
		}
	}

	/// Returns `true` if we are in warp-only mode.
	///
	/// i.e. we will never fall back to regular sync
	/// until given block number is reached by
	/// successfuly finding and restoring from a snapshot.
	pub fn is_warp_only(&self) -> bool {
		if let WarpSync::OnlyAndAfter(_) = *self {
			true
		} else {
			false
		}
	}
}

/// Current sync status
pub trait SyncProvider: Send + Sync {
	/// Get sync status
	fn status(&self) -> EthSyncStatus;

	/// Get peers information
	fn peers(&self) -> Vec<PeerInfo>;

	/// Get the enode if available.
	fn enode(&self) -> Option<String>;

	/// Returns propagation count for pending transactions.
	fn transactions_stats(&self) -> BTreeMap<H256, TransactionStats>;
}

/// Sync configuration
#[derive(Debug, Clone, Copy)]
pub struct SyncConfig {
	/// Max blocks to download ahead
	pub max_download_ahead_blocks: usize,
	/// Enable ancient block download.
	pub download_old_blocks: bool,
	/// Network ID
	pub network_id: u64,
	/// Main "eth" subprotocol name.
	pub subprotocol_name: [u8; 3],
	/// Light subprotocol name.
	pub light_subprotocol_name: [u8; 3],
	/// Fork block to check
	pub fork_block: Option<(BlockNumber, H256)>,
	/// Enable snapshot sync
	pub warp_sync: WarpSync,
	/// Enable light client server.
	pub serve_light: bool,
}

impl Default for SyncConfig {
	fn default() -> SyncConfig {
		SyncConfig {
			max_download_ahead_blocks: 20000,
			download_old_blocks: true,
			network_id: 1,
			subprotocol_name: ETH_PROTOCOL,
			light_subprotocol_name: LIGHT_PROTOCOL,
			fork_block: None,
			warp_sync: WarpSync::Disabled,
			serve_light: false,
		}
	}
}

/// EthSync initialization parameters.
pub struct Params {
	/// Configuration.
	pub config: SyncConfig,
	/// Blockchain client.
	pub chain: Arc<BlockChainClient>,
	/// Snapshot service.
	pub snapshot_service: Arc<SnapshotService>,
	/// Private tx service.
	pub private_tx_handler: Arc<PrivateTxHandler>,
	/// Light data provider.
	pub provider: Arc<::light::Provider>,
	/// Network layer configuration.
	pub network_config: NetworkConfiguration,
	/// Other protocols to attach.
	pub attached_protos: Vec<AttachedProtocol>,
}

/// Ethereum network protocol handler
pub struct EthSync {
	/// Network service
	network: NetworkService,
	/// Main (eth/par) protocol handler
	eth_handler: Arc<SyncProtocolHandler>,
	/// Light (pip) protocol handler
	light_proto: Option<Arc<LightProtocol>>,
	/// Other protocols to attach.
	attached_protos: Vec<AttachedProtocol>,
	/// The main subprotocol name
	subprotocol_name: [u8; 3],
	/// Light subprotocol name.
	light_subprotocol_name: [u8; 3],
}

fn light_params(
	network_id: u64,
	max_peers: u32,
	pruning_info: PruningInfo,
	sample_store: Option<Box<SampleStore>>,
) -> LightParams {
	const MAX_LIGHTSERV_LOAD: f64 = 0.5;

	let mut light_params = LightParams {
		network_id: network_id,
		config: Default::default(),
		capabilities: Capabilities {
			serve_headers: true,
			serve_chain_since: Some(pruning_info.earliest_chain),
			serve_state_since: Some(pruning_info.earliest_state),
			tx_relay: true,
		},
		sample_store: sample_store,
	};

	let max_peers = ::std::cmp::max(max_peers, 1);
	light_params.config.load_share = MAX_LIGHTSERV_LOAD / max_peers as f64;

	light_params
}

impl EthSync {
	/// Creates and register protocol with the network service
	pub fn new(params: Params, connection_filter: Option<Arc<ConnectionFilter>>) -> Result<Arc<EthSync>, Error> {
		let pruning_info = params.chain.pruning_info();
		let light_proto = match params.config.serve_light {
			false => None,
			true => Some({
				let sample_store = params.network_config.net_config_path
					.clone()
					.map(::std::path::PathBuf::from)
					.map(|mut p| { p.push("request_timings"); light_net::FileStore(p) })
					.map(|store| Box::new(store) as Box<_>);

				let light_params = light_params(
					params.config.network_id,
					params.network_config.max_peers,
					pruning_info,
					sample_store,
				);

				let mut light_proto = LightProtocol::new(params.provider, light_params);
				light_proto.add_handler(Arc::new(TxRelay(params.chain.clone())));

				Arc::new(light_proto)
			})
		};

		let chain_sync = ChainSync::new(params.config, &*params.chain, params.private_tx_handler.clone());
		let service = NetworkService::new(params.network_config.clone().into_basic()?, connection_filter)?;

		let sync = Arc::new(EthSync {
			network: service,
			eth_handler: Arc::new(SyncProtocolHandler {
				sync: RwLock::new(chain_sync),
				chain: params.chain,
				snapshot_service: params.snapshot_service,
				overlay: RwLock::new(HashMap::new()),
			}),
			light_proto: light_proto,
			subprotocol_name: params.config.subprotocol_name,
			light_subprotocol_name: params.config.light_subprotocol_name,
			attached_protos: params.attached_protos,
		});

		Ok(sync)
	}
}

impl SyncProvider for EthSync {
	/// Get sync status
	fn status(&self) -> EthSyncStatus {
		self.eth_handler.sync.read().status()
	}

	/// Get sync peers
	fn peers(&self) -> Vec<PeerInfo> {
		self.network.with_context_eval(self.subprotocol_name, |ctx| {
			let peer_ids = self.network.connected_peers();
			let eth_sync = self.eth_handler.sync.read();
			let light_proto = self.light_proto.as_ref();

			peer_ids.into_iter().filter_map(|peer_id| {
				let session_info = match ctx.session_info(peer_id) {
					None => return None,
					Some(info) => info,
				};

				Some(PeerInfo {
					id: session_info.id.map(|id| format!("{:x}", id)),
					client_version: session_info.client_version,
					capabilities: session_info.peer_capabilities.into_iter().map(|c| c.to_string()).collect(),
					remote_address: session_info.remote_address,
					local_address: session_info.local_address,
					eth_info: eth_sync.peer_info(&peer_id),
					pip_info: light_proto.as_ref().and_then(|lp| lp.peer_status(peer_id)).map(Into::into),
				})
			}).collect()
		}).unwrap_or_else(Vec::new)
	}

	fn enode(&self) -> Option<String> {
		self.network.external_url()
	}

	fn transactions_stats(&self) -> BTreeMap<H256, TransactionStats> {
		let sync = self.eth_handler.sync.read();
		sync.transactions_stats()
			.iter()
			.map(|(hash, stats)| (*hash, stats.into()))
			.collect()
	}
}


struct SyncProtocolHandler {
	/// Shared blockchain client.
	chain: Arc<BlockChainClient>,
	/// Shared snapshot service.
	snapshot_service: Arc<SnapshotService>,
	/// Sync strategy
	sync: RwLock<ChainSync>,
	/// Chain overlay used to cache data such as fork block.
	overlay: RwLock<HashMap<BlockNumber, Bytes>>,
}

impl NetworkProtocolHandler for SyncProtocolHandler {
	fn initialize(&self, io: &NetworkContext) {
		if io.subprotocol_name() != WARP_SYNC_PROTOCOL_ID {
			io.register_timer(PEERS_TIMER, Duration::from_millis(700)).expect("Error registering peers timer");
			io.register_timer(SYNC_TIMER, Duration::from_millis(1100)).expect("Error registering sync timer");
			io.register_timer(TX_TIMER, Duration::from_millis(1300)).expect("Error registering transactions timer");
		}
	}

	fn read(&self, io: &NetworkContext, peer: &PeerId, packet_id: u8, data: &[u8]) {
		ChainSync::dispatch_packet(&self.sync, &mut NetSyncIo::new(io, &*self.chain, &*self.snapshot_service, &self.overlay), *peer, packet_id, data);
	}

	fn connected(&self, io: &NetworkContext, peer: &PeerId) {
		trace_time!("sync::connected");
		// If warp protocol is supported only allow warp handshake
		let warp_protocol = io.protocol_version(WARP_SYNC_PROTOCOL_ID, *peer).unwrap_or(0) != 0;
		let warp_context = io.subprotocol_name() == WARP_SYNC_PROTOCOL_ID;
		if warp_protocol == warp_context {
			self.sync.write().on_peer_connected(&mut NetSyncIo::new(io, &*self.chain, &*self.snapshot_service, &self.overlay), *peer);
		}
	}

	fn disconnected(&self, io: &NetworkContext, peer: &PeerId) {
		trace_time!("sync::disconnected");
		if io.subprotocol_name() != WARP_SYNC_PROTOCOL_ID {
			self.sync.write().on_peer_aborting(&mut NetSyncIo::new(io, &*self.chain, &*self.snapshot_service, &self.overlay), *peer);
		}
	}

	fn timeout(&self, io: &NetworkContext, timer: TimerToken) {
		trace_time!("sync::timeout");
		let mut io = NetSyncIo::new(io, &*self.chain, &*self.snapshot_service, &self.overlay);
		match timer {
			PEERS_TIMER => self.sync.write().maintain_peers(&mut io),
			SYNC_TIMER => self.sync.write().maintain_sync(&mut io),
			TX_TIMER => {
				self.sync.write().propagate_new_transactions(&mut io);
			},
			_ => warn!("Unknown timer {} triggered.", timer),
		}
	}
}

impl ChainNotify for EthSync {
	fn new_blocks(&self,
		imported: Vec<H256>,
		invalid: Vec<H256>,
		route: ChainRoute,
		sealed: Vec<H256>,
		proposed: Vec<Bytes>,
		_duration: Duration)
	{
		use light::net::Announcement;

		self.network.with_context(self.subprotocol_name, |context| {
			let mut sync_io = NetSyncIo::new(context, &*self.eth_handler.chain, &*self.eth_handler.snapshot_service,
				&self.eth_handler.overlay);
			self.eth_handler.sync.write().chain_new_blocks(
				&mut sync_io,
				&imported,
				&invalid,
				route.enacted(),
				route.retracted(),
				&sealed,
				&proposed);
		});

		self.network.with_context(self.light_subprotocol_name, |context| {
			let light_proto = match self.light_proto.as_ref() {
				Some(lp) => lp,
				None => return,
			};

			let chain_info = self.eth_handler.chain.chain_info();
			light_proto.make_announcement(&context, Announcement {
				head_hash: chain_info.best_block_hash,
				head_num: chain_info.best_block_number,
				head_td: chain_info.total_difficulty,
				reorg_depth: 0, // recalculated on a per-peer basis.
				serve_headers: false, // these fields consist of _changes_ in capability.
				serve_state_since: None,
				serve_chain_since: None,
				tx_relay: false,
			})
		})
	}

	fn start(&self) {
		match self.network.start() {
			Err((err, listen_address)) => {
				match err.into() {
					ErrorKind::Io(ref e) if e.kind() == io::ErrorKind::AddrInUse => {
						warn!("Network port {:?} is already in use, make sure that another instance of an Ethereum client is not running or change the port using the --port option.", listen_address.expect("Listen address is not set."))
					},
					err => warn!("Error starting network: {}", err),
				}
			},
			_ => {},
		}

		self.network.register_protocol(self.eth_handler.clone(), self.subprotocol_name, &[ETH_PROTOCOL_VERSION_62, ETH_PROTOCOL_VERSION_63])
			.unwrap_or_else(|e| warn!("Error registering ethereum protocol: {:?}", e));
		// register the warp sync subprotocol
		self.network.register_protocol(self.eth_handler.clone(), WARP_SYNC_PROTOCOL_ID, &[PAR_PROTOCOL_VERSION_1, PAR_PROTOCOL_VERSION_2, PAR_PROTOCOL_VERSION_3])
			.unwrap_or_else(|e| warn!("Error registering snapshot sync protocol: {:?}", e));

		// register the light protocol.
		if let Some(light_proto) = self.light_proto.as_ref().map(|x| x.clone()) {
			self.network.register_protocol(light_proto, self.light_subprotocol_name, ::light::net::PROTOCOL_VERSIONS)
				.unwrap_or_else(|e| warn!("Error registering light client protocol: {:?}", e));
		}

		// register any attached protocols.
		for proto in &self.attached_protos { proto.register(&self.network) }
	}

	fn stop(&self) {
		self.eth_handler.snapshot_service.abort_restore();
		self.network.stop();
	}

	fn broadcast(&self, message_type: ChainMessageType) {
		self.network.with_context(WARP_SYNC_PROTOCOL_ID, |context| {
			let mut sync_io = NetSyncIo::new(context, &*self.eth_handler.chain, &*self.eth_handler.snapshot_service, &self.eth_handler.overlay);
			match message_type {
				ChainMessageType::Consensus(message) => self.eth_handler.sync.write().propagate_consensus_packet(&mut sync_io, message),
				ChainMessageType::PrivateTransaction(transaction_hash, message) =>
					self.eth_handler.sync.write().propagate_private_transaction(&mut sync_io, transaction_hash, PRIVATE_TRANSACTION_PACKET, message),
				ChainMessageType::SignedPrivateTransaction(transaction_hash, message) =>
					self.eth_handler.sync.write().propagate_private_transaction(&mut sync_io, transaction_hash, SIGNED_PRIVATE_TRANSACTION_PACKET, message),
			}
		});
	}

	fn transactions_received(&self, txs: &[UnverifiedTransaction], peer_id: PeerId) {
		let mut sync = self.eth_handler.sync.write();
		sync.transactions_received(txs, peer_id);
	}
}


/// PIP event handler.
/// Simply queues transactions from light client peers.
struct TxRelay(Arc<BlockChainClient>);

impl LightHandler for TxRelay {
	fn on_transactions(&self, ctx: &EventContext, relay: &[::transaction::UnverifiedTransaction]) {
		trace!(target: "pip", "Relaying {} transactions from peer {}", relay.len(), ctx.peer());
		self.0.queue_transactions(relay.iter().map(|tx| ::rlp::encode(tx).into_vec()).collect(), ctx.peer())
	}
}

impl ManageNetwork for EthSync {
	fn accept_unreserved_peers(&self) {
		self.network.set_non_reserved_mode(NonReservedPeerMode::Accept);
	}

	fn deny_unreserved_peers(&self) {
		self.network.set_non_reserved_mode(NonReservedPeerMode::Deny);
	}

	fn remove_reserved_peer(&self, peer: String) -> Result<(), String> {
		self.network.remove_reserved_peer(&peer).map_err(|e| format!("{:?}", e))
	}

	fn add_reserved_peer(&self, peer: String) -> Result<(), String> {
		self.network.add_reserved_peer(&peer).map_err(|e| format!("{:?}", e))
	}

	fn start_network(&self) {
		self.start();
	}

	fn stop_network(&self) {
		self.network.with_context(self.subprotocol_name, |context| {
			let mut sync_io = NetSyncIo::new(context, &*self.eth_handler.chain, &*self.eth_handler.snapshot_service, &self.eth_handler.overlay);
			self.eth_handler.sync.write().abort(&mut sync_io);
		});

		if let Some(light_proto) = self.light_proto.as_ref() {
			light_proto.abort();
		}

		self.stop();
	}

	fn num_peers_range(&self) -> Range<u32> {
		self.network.num_peers_range()
	}

	fn with_proto_context(&self, proto: ProtocolId, f: &mut FnMut(&NetworkContext)) {
		self.network.with_context_eval(proto, f);
	}
}


/// Configuration for IPC service.
#[derive(Debug, Clone)]
pub struct ServiceConfiguration {
	/// Sync config.
	pub sync: SyncConfig,
	/// Network configuration.
	pub net: NetworkConfiguration,
	/// IPC path.
	pub io_path: String,
}
