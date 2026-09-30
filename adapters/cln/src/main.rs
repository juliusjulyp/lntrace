//! Stage 1: CLN adapter plugin for lntrace.
//!
//! Subscribes to CLN notifications and translates them into lntrace
//! TraceEvent envelopes, written to a JSONL log via lntrace-collector.
//!
//! Notifications consumed:
//!   - forward_event       → TraceEvent::ForwardEvent
//!   - pay_part_start      → TraceEvent::PaymentPathAttempt
//!   - pay_part_end        → TraceEvent::PaymentPathResult
//!   - sendpay_success     → TraceEvent::PaymentSent
//!   - sendpay_failure     → TraceEvent::PaymentFailed
//!   - invoice_payment     → TraceEvent::PaymentReceived
//!   - channel_opened      → TraceEvent::ChannelReady
//!   - channel_state_changed → ChannelPending / ChannelClosed
//!
//! Install:
//!   cargo build -p lntrace-cln --bin lntrace-cln
//!   lightningd --plugin=/path/to/lntrace-cln

use anyhow::Result;
use cln_plugin::{Builder, Plugin};
use lntrace_cln::translate;
use lntrace_collector::LogHandle;
use lntrace_core::*;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone)]
struct State {
    node_id: NodeId,
    seq: Arc<AtomicU64>,
    log: LogHandle,
}

impl State {
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    async fn emit(&self, event: TraceEvent) -> Result<()> {
        let envelope = Envelope::new(self.node_id.clone(), self.next_seq(), event);
        self.log.ingest(envelope).await
    }
}

// --- Notification handlers ---
// Each handler delegates to translate::translate() and emits the result.
// Errors are logged and swallowed so a malformed notification never kills the plugin.

macro_rules! notification_handler {
    ($fn_name:ident, $topic:expr) => {
        async fn $fn_name(p: Plugin<State>, v: Value) -> Result<()> {
            match translate::translate($topic, &v) {
                Some(event) => {
                    if let Err(e) = p.state().emit(event).await {
                        eprintln!("lntrace: failed to write {}: {e}", $topic);
                    }
                }
                None => {} // notification we don't care about (e.g. intermediate channel state)
            }
            Ok(())
        }
    };
}

notification_handler!(on_forward_event, "forward_event");
notification_handler!(on_pay_part_start, "pay_part_start");
notification_handler!(on_pay_part_end, "pay_part_end");
notification_handler!(on_sendpay_success, "sendpay_success");
notification_handler!(on_sendpay_failure, "sendpay_failure");
notification_handler!(on_invoice_payment, "invoice_payment");
notification_handler!(on_channel_opened, "channel_opened");
notification_handler!(on_channel_state_changed, "channel_state_changed");

#[tokio::main]
async fn main() -> Result<()> {
    // The log writer runs in a background task. The plugin handlers
    // send envelopes through the LogHandle.
    let (writer, log_handle) = lntrace_collector::LogWriter::new("events.jsonl");
    tokio::spawn(async move {
        if let Err(e) = writer.run().await {
            eprintln!("lntrace log writer error: {e}");
        }
    });

    // The node_id is not known until after getinfo. Use a placeholder
    // and update it in the init callback (or accept it as an option).
    let state = State {
        node_id: NodeId("unknown".to_string()),
        seq: Arc::new(AtomicU64::new(0)),
        log: log_handle,
    };

    if let Some(plugin) = Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .subscribe("forward_event", on_forward_event)
        .subscribe("pay_part_start", on_pay_part_start)
        .subscribe("pay_part_end", on_pay_part_end)
        .subscribe("sendpay_success", on_sendpay_success)
        .subscribe("sendpay_failure", on_sendpay_failure)
        .subscribe("invoice_payment", on_invoice_payment)
        .subscribe("channel_opened", on_channel_opened)
        .subscribe("channel_state_changed", on_channel_state_changed)
        .start(state)
        .await?
    {
        plugin.join().await?;
    }
    Ok(())
}
