//! LDK Node adapter for lntrace.
//!
//! Translates `ldk_node::Event` values into lntrace `TraceEvent` envelopes.
//! The adapter observes only — it never consumes events from LDK Node's
//! event queue.
//!
//! Capabilities (from the README capability matrix):
//!   - channel_lifecycle: yes
//!   - forward_settled_failed: settled only (no failed forward events)
//!   - All sender-side and in-flight capabilities: no
//!
//! The app keeps ownership of the LDK Node event loop. Usage:
//!
//! ```rust,ignore
//! let adapter = LdkAdapter::new(node_id, log_handle);
//!
//! loop {
//!     let event = node.next_event_async().await;
//!     adapter.record(&event).await;  // forward a copy to lntrace
//!     handle(event);                 // your existing logic
//!     node.event_handled();
//! }
//! ```

use lntrace_collector::LogHandle;
use lntrace_core::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[allow(dead_code)]
pub struct LdkAdapter {
    node_id: NodeId,
    seq: Arc<AtomicU64>,
    log: LogHandle,
}

impl LdkAdapter {
    pub fn new(node_id: NodeId, log: LogHandle) -> Self {
        LdkAdapter {
            node_id,
            seq: Arc::new(AtomicU64::new(0)),
            log,
        }
    }

    #[allow(dead_code)]
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    #[allow(dead_code)]
    async fn emit(&self, event: TraceEvent) -> anyhow::Result<()> {
        let envelope = Envelope::new(self.node_id.clone(), self.next_seq(), event);
        self.log.ingest(envelope).await
    }

    /// Record an LDK Node event. Call this before your own event handler.
    ///
    /// Translates the subset of events that lntrace cares about and
    /// silently ignores the rest.
    ///
    /// NOTE: This method accepts a serde_json::Value for now.
    /// When ldk-node is added as a dependency, replace this with
    /// the typed ldk_node::Event enum.
    pub async fn record(&self, event: &serde_json::Value) -> anyhow::Result<()> {
        // TODO(stage-3): Match on ldk_node::Event variants:
        //   Event::ChannelPending { .. }  → TraceEvent::ChannelPending
        //   Event::ChannelReady { .. }    → TraceEvent::ChannelReady
        //   Event::ChannelClosed { .. }   → TraceEvent::ChannelClosed
        //   Event::PaymentSuccessful { .. } → TraceEvent::PaymentSent
        //   Event::PaymentFailed { .. }     → TraceEvent::PaymentFailed
        //   Event::PaymentReceived { .. }   → TraceEvent::PaymentReceived
        //   Event::PaymentForwarded { .. }  → TraceEvent::ForwardEvent (settled only)
        let _ = event;
        Ok(())
    }
}
