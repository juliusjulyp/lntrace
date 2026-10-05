//! Stage 1: CLN adapter plugin for lntrace.
//!
//! Subscribes to CLN notifications and translates them into lntrace
//! TraceEvent envelopes, written to a JSONL log via lntrace-collector.
//!
//! Polls `listpeerchannels` periodically and on demand after `local_failed`
//! forward events, emitting Snapshot events for cause inference.
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
use cln_rpc::model::requests::ListpeerchannelsRequest;
use lntrace::{Envelope, LogHandle, NodeId, TraceEvent};
use lntrace_cln::translate;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone)]
struct State {
    node_id: NodeId,
    seq: Arc<AtomicU64>,
    log: LogHandle,
    /// Trigger an immediate listpeerchannels poll.
    poll_trigger: Arc<Notify>,
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

macro_rules! notification_handler {
    ($fn_name:ident, $topic:expr) => {
        async fn $fn_name(p: Plugin<State>, v: Value) -> Result<()> {
            match translate::translate($topic, &v) {
                Some(event) => {
                    if let Err(e) = p.state().emit(event).await {
                        eprintln!("lntrace: failed to write {}: {e}", $topic);
                    }
                }
                None => {}
            }
            Ok(())
        }
    };
}

/// forward_event handler with poll trigger on local_failed.
async fn on_forward_event(p: Plugin<State>, v: Value) -> Result<()> {
    let is_local_failed = v["forward_event"]["status"]
        .as_str()
        .map_or(false, |s| s == "local_failed");

    match translate::translate("forward_event", &v) {
        Some(event) => {
            if let Err(e) = p.state().emit(event).await {
                eprintln!("lntrace: failed to write forward_event: {e}");
            }
        }
        None => {}
    }

    // Trigger an immediate snapshot after local_failed to capture channel state.
    if is_local_failed {
        p.state().poll_trigger.notify_one();
    }

    Ok(())
}

notification_handler!(on_pay_part_start, "pay_part_start");
notification_handler!(on_pay_part_end, "pay_part_end");
notification_handler!(on_sendpay_success, "sendpay_success");
notification_handler!(on_sendpay_failure, "sendpay_failure");
notification_handler!(on_invoice_payment, "invoice_payment");
notification_handler!(on_channel_opened, "channel_opened");
notification_handler!(on_channel_state_changed, "channel_state_changed");

/// Background task that polls listpeerchannels periodically and on demand.
///
/// Full snapshot at startup and after triggered polls (e.g. local_failed).
/// Periodic polls only emit channels that changed since the last snapshot.
async fn poll_channels(rpc_path: PathBuf, state: State, poll_interval_secs: u64) {
    let mut rpc = match cln_rpc::ClnRpc::new(&rpc_path).await {
        Ok(rpc) => rpc,
        Err(e) => {
            eprintln!("lntrace: RPC connect for polling failed: {e}");
            return;
        }
    };

    let mut previous: HashMap<Option<lntrace::ShortChannelId>, lntrace::ChannelSnapshot> =
        HashMap::new();

    // Initial full snapshot at startup.
    do_poll(&mut rpc, &state, &mut previous, true).await;

    loop {
        let triggered = tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval_secs)) => false,
            _ = state.poll_trigger.notified() => true,
        };
        do_poll(&mut rpc, &state, &mut previous, triggered).await;
    }
}

async fn do_poll(
    rpc: &mut cln_rpc::ClnRpc,
    state: &State,
    previous: &mut HashMap<Option<lntrace::ShortChannelId>, lntrace::ChannelSnapshot>,
    full: bool,
) {
    let req = ListpeerchannelsRequest {
        id: None,
        channel_id: None,
        short_channel_id: None,
    };
    match rpc.call_typed(&req).await {
        Ok(resp) => {
            let channels = translate::translate_listpeerchannels(&resp);
            if channels.is_empty() {
                return;
            }

            let to_emit = if full {
                // Full snapshot: emit everything.
                channels.clone()
            } else {
                // Diff: only channels that changed.
                channels
                    .iter()
                    .filter(|ch| {
                        previous
                            .get(&ch.channel.scid)
                            .map_or(true, |prev| prev != *ch)
                    })
                    .cloned()
                    .collect()
            };

            // Update previous state.
            for ch in &channels {
                previous.insert(ch.channel.scid, ch.clone());
            }

            if !to_emit.is_empty() {
                if let Err(e) = state.emit(TraceEvent::Snapshot { channels: to_emit }).await {
                    eprintln!("lntrace: failed to write snapshot: {e}");
                }
            }
        }
        Err(e) => {
            eprintln!("lntrace: listpeerchannels poll failed: {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (writer, log_handle) = lntrace::LogWriter::new("events.jsonl");
    tokio::spawn(async move {
        if let Err(e) = writer.run().await {
            eprintln!("lntrace log writer error: {e}");
        }
    });

    let state = State {
        node_id: NodeId("unknown".to_string()),
        seq: Arc::new(AtomicU64::new(0)),
        log: log_handle,
        poll_trigger: Arc::new(Notify::new()),
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
        // Get RPC path from plugin configuration.
        let config = plugin.configuration();
        let rpc_path = PathBuf::from(&config.lightning_dir).join(&config.rpc_file);

        // Resolve node_id via getinfo.
        let rpc_path_getinfo = rpc_path.clone();
        tokio::spawn(async move {
            match cln_rpc::ClnRpc::new(&rpc_path_getinfo).await {
                Ok(mut rpc) => {
                    let req = cln_rpc::model::requests::GetinfoRequest {};
                    match rpc.call_typed(&req).await {
                        Ok(info) => {
                            // State has NodeId by value — we can't update it here.
                            // Node ID was set at construction time. For now, log it.
                            eprintln!("lntrace: adapter started for {}", info.id);
                        }
                        Err(e) => eprintln!("lntrace: getinfo failed: {e}"),
                    }
                }
                Err(e) => eprintln!("lntrace: RPC connect failed: {e}"),
            }
        });

        // Start channel polling (default: every 2 seconds).
        let poll_state = plugin.state().clone();
        tokio::spawn(poll_channels(rpc_path, poll_state, 2));

        plugin.join().await?;
    }
    Ok(())
}
