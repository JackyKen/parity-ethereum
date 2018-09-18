// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use std::collections::{HashMap, BTreeMap};
use std::net::{SocketAddr, AddrParseError};
use std::str::FromStr;
use std::sync::Arc;

use ethcore::ethstore::ethkey::Secret;
use ethereum_types::{H256, H512, U256};
use devp2p::NetworkService;
use light::net::{self as light_net};
use network::{NetworkProtocolHandler, ProtocolId,
	NetworkConfiguration as BasicNetworkConfiguration, NonReservedPeerMode,
	IpFilter};

/// Parity sync protocol
pub const WARP_SYNC_PROTOCOL_ID: ProtocolId = *b"par";
/// Ethereum sync protocol
pub const ETH_PROTOCOL: ProtocolId = *b"eth";
/// Ethereum light protocol
pub const LIGHT_PROTOCOL: ProtocolId = *b"pip";


/// Transaction stats
#[derive(Debug)]
pub struct TransactionStats {
	/// Block number where this TX was first seen.
	pub first_seen: u64,
	/// Peers it was propagated to.
	pub propagated_to: BTreeMap<H512, usize>,
}

/// Peer connection information
#[derive(Debug)]
pub struct PeerInfo {
	/// Public node id
	pub id: Option<String>,
	/// Node client ID
	pub client_version: String,
	/// Capabilities
	pub capabilities: Vec<String>,
	/// Remote endpoint address
	pub remote_address: String,
	/// Local endpoint address
	pub local_address: String,
	/// Eth protocol info.
	pub eth_info: Option<EthProtocolInfo>,
	/// Light protocol info.
	pub pip_info: Option<PipProtocolInfo>,
}

/// Ethereum protocol info.
#[derive(Debug)]
pub struct EthProtocolInfo {
	/// Protocol version
	pub version: u32,
	/// SHA3 of peer best block hash
	pub head: H256,
	/// Peer total difficulty if known
	pub difficulty: Option<U256>,
}

/// PIP protocol info.
#[derive(Debug)]
pub struct PipProtocolInfo {
	/// Protocol version
	pub version: u32,
	/// SHA3 of peer best block hash
	pub head: H256,
	/// Peer total difficulty if known
	pub difficulty: U256,
}

/// Configuration to attach alternate protocol handlers.
/// Only works when IPC is disabled.
pub struct AttachedProtocol {
	/// The protocol handler in question.
	pub handler: Arc<NetworkProtocolHandler + Send + Sync>,
	/// 3-character ID for the protocol.
	pub protocol_id: ProtocolId,
	/// Supported versions and their packet counts.
	pub versions: &'static [(u8, u8)],
}

impl AttachedProtocol {
	/// Register a network service
	pub (crate) fn register(&self, network: &NetworkService) {
		let res = network.register_protocol(
			self.handler.clone(),
			self.protocol_id,
			self.versions
		);

		if let Err(e) = res {
			warn!(target: "sync", "Error attaching protocol {:?}: {:?}", self.protocol_id, e);
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Network service configuration
pub struct NetworkConfiguration {
	/// Directory path to store general network configuration. None means nothing will be saved
	pub config_path: Option<String>,
	/// Directory path to store network-specific configuration. None means nothing will be saved
	pub net_config_path: Option<String>,
	/// IP address to listen for incoming connections. Listen to all connections by default
	pub listen_address: Option<String>,
	/// IP address to advertise. Detected automatically if none.
	pub public_address: Option<String>,
	/// Port for UDP connections, same as TCP by default
	pub udp_port: Option<u16>,
	/// Enable NAT configuration
	pub nat_enabled: bool,
	/// Enable discovery
	pub discovery_enabled: bool,
	/// List of initial node addresses
	pub boot_nodes: Vec<String>,
	/// Use provided node key instead of default
	pub use_secret: Option<Secret>,
	/// Max number of connected peers to maintain
	pub max_peers: u32,
	/// Min number of connected peers to maintain
	pub min_peers: u32,
	/// Max pending peers.
	pub max_pending_peers: u32,
	/// Reserved snapshot sync peers.
	pub snapshot_peers: u32,
	/// List of reserved node addresses.
	pub reserved_nodes: Vec<String>,
	/// The non-reserved peer mode.
	pub allow_non_reserved: bool,
	/// IP Filtering
	pub ip_filter: IpFilter,
	/// Client version string
	pub client_version: String,
}

impl NetworkConfiguration {
	/// Create a new default config.
	pub fn new() -> Self {
		From::from(BasicNetworkConfiguration::new())
	}

	/// Create a new local config.
	pub fn new_local() -> Self {
		From::from(BasicNetworkConfiguration::new_local())
	}

	/// Attempt to convert this config into a BasicNetworkConfiguration.
	pub fn into_basic(self) -> Result<BasicNetworkConfiguration, AddrParseError> {
		Ok(BasicNetworkConfiguration {
			config_path: self.config_path,
			net_config_path: self.net_config_path,
			listen_address: match self.listen_address { None => None, Some(addr) => Some(SocketAddr::from_str(&addr)?) },
			public_address: match self.public_address { None => None, Some(addr) => Some(SocketAddr::from_str(&addr)?) },
			udp_port: self.udp_port,
			nat_enabled: self.nat_enabled,
			discovery_enabled: self.discovery_enabled,
			boot_nodes: self.boot_nodes,
			use_secret: self.use_secret,
			max_peers: self.max_peers,
			min_peers: self.min_peers,
			max_handshakes: self.max_pending_peers,
			reserved_protocols: hash_map![WARP_SYNC_PROTOCOL_ID => self.snapshot_peers],
			reserved_nodes: self.reserved_nodes,
			ip_filter: self.ip_filter,
			non_reserved_mode: if self.allow_non_reserved { NonReservedPeerMode::Accept } else { NonReservedPeerMode::Deny },
			client_version: self.client_version,
		})
	}
}

impl From<BasicNetworkConfiguration> for NetworkConfiguration {
	fn from(other: BasicNetworkConfiguration) -> Self {
		NetworkConfiguration {
			config_path: other.config_path,
			net_config_path: other.net_config_path,
			listen_address: other.listen_address.and_then(|addr| Some(format!("{}", addr))),
			public_address: other.public_address.and_then(|addr| Some(format!("{}", addr))),
			udp_port: other.udp_port,
			nat_enabled: other.nat_enabled,
			discovery_enabled: other.discovery_enabled,
			boot_nodes: other.boot_nodes,
			use_secret: other.use_secret,
			max_peers: other.max_peers,
			min_peers: other.min_peers,
			max_pending_peers: other.max_handshakes,
			snapshot_peers: *other.reserved_protocols.get(&WARP_SYNC_PROTOCOL_ID).unwrap_or(&0),
			reserved_nodes: other.reserved_nodes,
			ip_filter: other.ip_filter,
			allow_non_reserved: match other.non_reserved_mode { NonReservedPeerMode::Accept => true, _ => false } ,
			client_version: other.client_version,
		}
	}
}

/// Numbers of peers (max, min, active).
#[derive(Debug, Clone)]
pub struct PeerNumbers {
	/// Number of connected peers.
	pub connected: usize,
	/// Number of active peers.
	pub active: usize,
	/// Max peers.
	pub max: usize,
	/// Min peers.
	pub min: usize,
}

impl From<light_net::Status> for PipProtocolInfo {
        fn from(status: light_net::Status) -> Self {
                PipProtocolInfo {
                        version: status.protocol_version,
                        head: status.head_hash,
                        difficulty: status.head_td,
                }
        }
}
