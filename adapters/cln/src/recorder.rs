//! lntrace CLN plugin.
//!
//! A CLN plugin that subscribes to all payment-relevant notifications
//! and records the raw JSON to a JSONL file. Also polls `listpeerchannels`
//! periodically and after `local_failed` forward events.
//!
//! Each line includes `node_id` and `alias` so logs from multiple
//! nodes can be merged or compared.
//!
//! Install:
//!   cargo build -p lntrace-cln --release
//!   lightningd --plugin=/path/to/lntrace-cln
//!
//! Options:
//!   --lntrace-log=PATH   Write to a specific file.
//!   --lntrace-dir=DIR    Write to DIR/{alias}.jsonl (for multi-node setups).
//!
//! Default output: `lntrace-raw.jsonl` in the CLN data directory.

use anyhow::Result;
use cln_plugin::options::StringConfigOption;
use cln_plugin::{Builder, Plugin};
use log::{error, info, warn};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify};

/// CLN option: full path for the output file.
/// When not set, defaults to `lntrace-raw.jsonl` in the CLN data directory.
const OPT_LOG: StringConfigOption = StringConfigOption::new_str_no_default(
    "lntrace-log",
    "Output path for lntrace raw JSONL (default: lntrace-raw.jsonl in CLN dir)",
);

/// CLN option: directory for output file, auto-named by alias.
/// When set, the file is written to `{dir}/{alias}.jsonl`.
/// Useful for multi-node setups where every node points at the same directory.
const OPT_DIR: StringConfigOption = StringConfigOption::new_str_no_default(
    "lntrace-dir",
    "Output directory for lntrace (file named {alias}.jsonl)",
);

#[derive(Clone)]
struct State {
    path: Arc<Mutex<Option<PathBuf>>>,
    /// Buffered events recorded before the path is resolved.
    buffer: Arc<Mutex<Vec<String>>>,
    /// Node public key, set after init via getinfo RPC.
    node_id: Arc<Mutex<String>>,
    /// Node alias from lightningd config.
    alias: Arc<Mutex<String>>,
    /// Trigger an immediate listpeerchannels poll.
    poll_trigger: Arc<Notify>,
}

impl State {
    async fn record(&self, topic: &str, payload: &Value) -> Result<()> {
        let ts_ms = lntrace::now_ms();
        let node_id = self.node_id.lock().await.clone();
        let alias = self.alias.lock().await.clone();
        let line = serde_json::json!({
            "ts_ms": ts_ms,
            "topic": topic,
            "node_id": node_id,
            "alias": alias,
            "payload": payload,
        });

        let mut buf = serde_json::to_string(&line)?;
        buf.push('\n');

        let path = self.path.lock().await;
        if let Some(ref path) = *path {
            // Path is resolved — write directly.
            self.flush_buffer(path).await;
            write_line(path, buf.as_bytes()).await?;
        } else {
            // Path not yet resolved — buffer the line.
            let mut buffer = self.buffer.lock().await;
            if buffer.len() >= 1000 {
                warn!("startup buffer full (1000 events), dropping event");
            } else {
                buffer.push(buf);
            }
        }
        Ok(())
    }

    /// Flush any buffered lines to the resolved path.
    async fn flush_buffer(&self, path: &PathBuf) {
        let mut buffer = self.buffer.lock().await;
        if buffer.is_empty() {
            return;
        }
        for line in buffer.drain(..) {
            if let Err(e) = write_line(path, line.as_bytes()).await {
                warn!("failed to flush buffered event: {e}");
            }
        }
    }
}

/// Open-write-close on each call so that external truncation or
/// deletion (e.g. `rm -f` on the host) doesn't leave us writing
/// to a phantom inode.
async fn write_line(path: &PathBuf, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(data).await?;
    file.flush().await?;
    Ok(())
}

// --- Notification handlers ---

macro_rules! record_handler {
    ($fn_name:ident, $topic:expr) => {
        async fn $fn_name(p: Plugin<State>, v: Value) -> Result<()> {
            p.state().record($topic, &v).await
        }
    };
}

/// forward_event handler with poll trigger on local_failed.
async fn on_forward_event(p: Plugin<State>, v: Value) -> Result<()> {
    let is_local_failed = v["forward_event"]["status"].as_str() == Some("local_failed");

    p.state().record("forward_event", &v).await?;

    if is_local_failed {
        p.state().poll_trigger.notify_one();
    }

    Ok(())
}

record_handler!(on_sendpay_success, "sendpay_success");
record_handler!(on_sendpay_failure, "sendpay_failure");
record_handler!(on_pay_part_start, "pay_part_start");
record_handler!(on_pay_part_end, "pay_part_end");
record_handler!(on_channel_opened, "channel_opened");
record_handler!(on_channel_state_changed, "channel_state_changed");
record_handler!(on_invoice_payment, "invoice_payment");

/// Background task that polls listpeerchannels and records the response.
///
/// Full record at startup and after triggered polls (e.g. local_failed).
/// Periodic polls only record when the response changed since last poll.
async fn poll_channels(rpc_path: PathBuf, state: State, poll_interval_secs: u64) {
    let mut rpc = match cln_rpc::ClnRpc::new(&rpc_path).await {
        Ok(rpc) => rpc,
        Err(e) => {
            error!("RPC connect for polling failed: {e}");
            return;
        }
    };

    let mut previous_json: Option<String> = None;

    // Initial poll at startup — always record.
    do_poll(&mut rpc, &state, &mut previous_json, true).await;

    loop {
        let triggered = tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval_secs)) => false,
            _ = state.poll_trigger.notified() => true,
        };
        do_poll(&mut rpc, &state, &mut previous_json, triggered).await;
    }
}

async fn do_poll(
    rpc: &mut cln_rpc::ClnRpc,
    state: &State,
    previous_json: &mut Option<String>,
    force: bool,
) {
    match lntrace_cln::poller::list_peer_channels(rpc).await {
        Ok(resp) => {
            let payload = serde_json::to_value(&resp).unwrap_or_default();
            let current_json = serde_json::to_string(&payload).unwrap_or_default();

            // On periodic polls, skip if nothing changed.
            if !force && previous_json.as_ref() == Some(&current_json) {
                return;
            }

            previous_json.replace(current_json);

            if let Err(e) = state.record("listpeerchannels", &payload).await {
                error!("failed to record listpeerchannels: {e}");
            }
        }
        Err(e) => {
            error!("listpeerchannels poll failed: {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = State {
        path: Arc::new(Mutex::new(None)),
        buffer: Arc::new(Mutex::new(Vec::new())),
        node_id: Arc::new(Mutex::new("pending".to_string())),
        alias: Arc::new(Mutex::new("pending".to_string())),
        poll_trigger: Arc::new(Notify::new()),
    };

    if let Some(plugin) = Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .option(OPT_LOG)
        .option(OPT_DIR)
        .subscribe("forward_event", on_forward_event)
        .subscribe("sendpay_success", on_sendpay_success)
        .subscribe("sendpay_failure", on_sendpay_failure)
        .subscribe("pay_part_start", on_pay_part_start)
        .subscribe("pay_part_end", on_pay_part_end)
        .subscribe("channel_opened", on_channel_opened)
        .subscribe("channel_state_changed", on_channel_state_changed)
        .subscribe("invoice_payment", on_invoice_payment)
        .start(state)
        .await?
    {
        let config = plugin.configuration();
        let rpc_path = PathBuf::from(&config.lightning_dir).join(&config.rpc_file);

        // Resolve node identity via getinfo RPC before resolving the log
        // path, so --lntrace-dir can use the alias for the filename.
        let mut node_alias = String::new();
        match cln_rpc::ClnRpc::new(&rpc_path).await {
            Ok(mut rpc) => {
                let req = cln_rpc::model::requests::GetinfoRequest {};
                match rpc.call_typed(&req).await {
                    Ok(info) => {
                        *plugin.state().node_id.lock().await = info.id.to_string();
                        node_alias = info.alias.clone();
                        *plugin.state().alias.lock().await = info.alias;
                    }
                    Err(e) => warn!("getinfo failed: {e}"),
                }
            }
            Err(e) => warn!("RPC connect failed: {e}"),
        }

        // Resolve the output path. Priority:
        //   1. --lntrace-log (explicit full path)
        //   2. --lntrace-dir (directory + auto-named file)
        //   3. default: {lightning_dir}/lntrace-raw.jsonl
        let log_path = if let Ok(Some(p)) = plugin.option(&OPT_LOG) {
            PathBuf::from(p)
        } else if let Ok(Some(dir)) = plugin.option(&OPT_DIR) {
            let filename = if node_alias.is_empty() {
                "lntrace-raw.jsonl".to_string()
            } else {
                format!("{}.jsonl", node_alias)
            };
            PathBuf::from(dir).join(filename)
        } else {
            PathBuf::from(&config.lightning_dir).join("lntrace-raw.jsonl")
        };

        // Ensure parent directory exists.
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Set the path — this also triggers flushing any buffered events
        // that arrived between plugin start and now.
        *plugin.state().path.lock().await = Some(log_path.clone());
        plugin.state().flush_buffer(&log_path).await;

        info!("recording to {}", log_path.display());

        // Start channel polling (every 2 seconds).
        let poll_state = plugin.state().clone();
        tokio::spawn(poll_channels(rpc_path, poll_state, 2));

        plugin.join().await?;
    }
    Ok(())
}
