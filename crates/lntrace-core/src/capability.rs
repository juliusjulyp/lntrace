use serde::{Deserialize, Serialize};

/// Declares what an adapter can provide.
///
/// Missing capabilities degrade gracefully: the trace still works,
/// with gaps marked. The correlator, CLI, and UI all read these
/// to decide what to show and what to flag as unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub channel_lifecycle: bool,
    pub forward_in_flight: bool,
    pub forward_settled_failed: bool,
    pub payment_hash_on_forwards: bool,
    pub sender_per_path: bool,
    pub sender_failing_hop: bool,
    pub sender_mpp_detail: bool,
    pub in_flight_htlc: bool,
}

impl Capabilities {
    /// LDK Node: intermediate/receiver only. No sender-side or in-flight data.
    pub fn ldk_node() -> Self {
        Capabilities {
            channel_lifecycle: true,
            forward_in_flight: false,
            forward_settled_failed: true, // settled only; no failed forward events
            payment_hash_on_forwards: false, // inferred by correlator
            sender_per_path: false,
            sender_failing_hop: false,
            sender_mpp_detail: false,
            in_flight_htlc: false,
        }
    }

    /// Core Lightning: full sender via xpay notifications.
    pub fn cln() -> Self {
        Capabilities {
            channel_lifecycle: true,
            forward_in_flight: true,
            forward_settled_failed: true,
            payment_hash_on_forwards: true,
            sender_per_path: true,    // pay_part_start
            sender_failing_hop: true, // pay_part_end: failed_node_id
            sender_mpp_detail: true,  // groupid + partid
            in_flight_htlc: true,
        }
    }

    /// LND: full sender via routerrpc (planned for v0.2 adapter).
    pub fn lnd() -> Self {
        Capabilities {
            channel_lifecycle: true,
            forward_in_flight: true, // SubscribeHtlcEvents
            forward_settled_failed: true,
            payment_hash_on_forwards: false, // via settle preimage only
            sender_per_path: true,           // TrackPayments
            sender_failing_hop: true,        // failure_source_index
            sender_mpp_detail: true,         // HTLC attempts per payment
            in_flight_htlc: true,
        }
    }
}
