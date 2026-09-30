use crate::event::TraceEvent;
use crate::types::NodeId;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Every event is wrapped in an envelope for transport and storage.
///
/// The adapter fills in `node_id`, `seq`, `node_ts_ms`, and `event`.
/// The collector stamps `collector_ts_ms` on receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// The node that observed this event.
    pub node_id: NodeId,
    /// Per-node monotonic sequence number, assigned by the adapter.
    pub seq: u64,
    /// Node's wall-clock timestamp (milliseconds since Unix epoch).
    pub node_ts_ms: u64,
    /// Collector's receive timestamp (milliseconds since Unix epoch).
    /// `None` in transit from adapter to collector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_ts_ms: Option<u64>,
    /// The normalized event payload.
    pub event: TraceEvent,
}

impl Envelope {
    /// Create a new envelope. The adapter must supply the sequence number.
    pub fn new(node_id: NodeId, seq: u64, event: TraceEvent) -> Self {
        Envelope {
            schema_version: SCHEMA_VERSION,
            node_id,
            seq,
            node_ts_ms: now_ms(),
            collector_ts_ms: None,
            event,
        }
    }

    /// Stamp the collector receive time.
    pub fn stamp_collector_ts(&mut self) {
        self.collector_ts_ms = Some(now_ms());
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as u64
}
