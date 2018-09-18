use std::collections::BTreeMap;

use ethereum_types::H256;
use light_sync::{PeerNumbers, PeerInfo, TransactionStats};

/// Light synchronization.
pub trait LightSyncProvider {
	/// Get peer numbers.
	fn peer_numbers(&self) -> PeerNumbers;

	/// Get peers information
	fn peers(&self) -> Vec<PeerInfo>;

	/// Get network id.
	fn network_id(&self) -> u64;

	/// Get the enode if available.
	fn enode(&self) -> Option<String>;

	/// Returns propagation count for pending transactions.
	fn transactions_stats(&self) -> BTreeMap<H256, TransactionStats>;
}

/// Trait for erasing the type of a light sync object and exposing read-only methods.
pub trait SyncInfo {
	/// Get the highest block advertised on the network.
	fn highest_block(&self) -> Option<u64>;

	/// Get the block number at the time of sync start.
	fn start_block(&self) -> u64;

	/// Whether major sync is underway.
	fn is_major_importing(&self) -> bool;

	/// Whether major sync is underway, skipping some synchronization.
	fn is_major_importing_no_sync(&self) -> bool;
}

