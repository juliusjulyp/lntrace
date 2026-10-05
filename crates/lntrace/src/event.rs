use crate::types::*;
use serde::{Deserialize, Serialize};

/// Normalized trace event. Implementation-agnostic.
///
/// Every variant maps to one row in the event schema table in the README.
/// Adapters translate implementation-specific events into these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraceEvent {
    /// Full channel/balance state dump on adapter connect (polled).
    Snapshot { channels: Vec<ChannelSnapshot> },

    /// Funding transaction broadcast.
    ChannelPending {
        channel: ChannelId,
        peer: NodeId,
        capacity_sat: u64,
    },

    /// Channel confirmed and usable.
    ChannelReady {
        channel: ChannelId,
        peer: NodeId,
        capacity_sat: u64,
    },

    /// Channel closed (cooperative, force, or breach).
    ChannelClosed {
        channel: ChannelId,
        peer: NodeId,
        reason: CloseReason,
    },

    /// Local/remote balance change (polled).
    BalanceUpdate {
        channel: ChannelId,
        local_msat: u64,
        remote_msat: u64,
    },

    /// HTLC forward event at an intermediate node.
    ForwardEvent {
        in_channel: ChannelId,
        out_channel: ChannelId,
        in_msat: u64,
        out_msat: u64,
        fee_msat: u64,
        status: ForwardStatus,
        /// Present when the implementation exposes it (CLN: yes, LDK: no).
        #[serde(skip_serializing_if = "Option::is_none")]
        payment_hash: Option<String>,
        /// BOLT 4 failure code, present on failed/local_failed.
        #[serde(skip_serializing_if = "Option::is_none")]
        failcode: Option<u16>,
        /// Human-readable failure reason from the implementation.
        /// CLN: `failreason` on `local_failed`. LND: `failure_detail`.
        #[serde(skip_serializing_if = "Option::is_none")]
        failreason: Option<String>,
    },

    /// Outbound payment completed (all implementations).
    PaymentSent {
        payment_hash: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payment_preimage: Option<String>,
        amount_msat: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        fee_msat: Option<u64>,
    },

    /// Outbound payment failed (all implementations).
    PaymentFailed {
        payment_hash: String,
        /// BOLT 4 failure code (CLN/LND). LDK: mapped from PaymentFailureReason.
        #[serde(skip_serializing_if = "Option::is_none")]
        failcode: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Inbound payment settled (all implementations).
    PaymentReceived {
        payment_hash: String,
        amount_msat: u64,
    },

    /// Individual path/shard dispatched with full route.
    /// Source: CLN `pay_part_start`, LND `TrackPayments`.
    PaymentPathAttempt {
        payment_hash: String,
        /// CLN shard key.
        #[serde(skip_serializing_if = "Option::is_none")]
        groupid: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        partid: Option<u64>,
        route: Vec<Hop>,
    },

    /// Individual path/shard settled or failed with failing hop.
    /// Source: CLN `pay_part_end`, LND `TrackPayments`.
    PaymentPathResult {
        payment_hash: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        groupid: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        partid: Option<u64>,
        success: bool,
        /// BOLT 4 failure code on failure.
        #[serde(skip_serializing_if = "Option::is_none")]
        failcode: Option<u16>,
        /// The node that returned the error.
        #[serde(skip_serializing_if = "Option::is_none")]
        erring_node: Option<NodeId>,
        /// The channel on which the error occurred.
        #[serde(skip_serializing_if = "Option::is_none")]
        erring_channel: Option<ChannelId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        /// Duration of the attempt in seconds.
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f64>,
        /// Direction of the failing channel (0 or 1).
        #[serde(skip_serializing_if = "Option::is_none")]
        failed_direction: Option<u32>,
    },
}

/// Channel state for Snapshot events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelSnapshot {
    pub channel: ChannelId,
    pub peer: NodeId,
    pub capacity_sat: u64,
    pub local_msat: u64,
    pub remote_msat: u64,
    pub active: bool,
    /// Channel direction (0 or 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<u32>,
    /// Implementation-specific channel state string (e.g. "CHANNELD_NORMAL").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// How much we can actually send right now (after reserves, fees, in-flight).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spendable_msat: Option<u64>,
    /// How much the peer can send to us right now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receivable_msat: Option<u64>,
    /// Minimum HTLC we can send through this channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_htlc_out_msat: Option<u64>,
    /// Maximum number of HTLCs allowed in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_accepted_htlcs: Option<u32>,
    /// Number of HTLCs currently in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inflight_htlc_count: Option<u32>,
}
