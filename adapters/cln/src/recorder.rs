//! Dev tool: raw CLN notification recorder.
//!
//! A CLN plugin that subscribes to all payment-relevant notifications
//! and dumps the raw JSON to a JSONL file. Also polls `listpeerchannels`
//! periodically and after `local_failed` forward events.
//!
//! Each line includes `node_id` and `alias` so logs from multiple
//! nodes can be merged or compared.
//!
//! Install:
//!   cargo build -p lntrace-cln --bin lntrace-cln-record
//!   lightningd --plugin=/path/to/lntrace-cln-record
//!
//! Output goes to `lntrace-raw.jsonl` in the CLN network data directory.

use anyhow::Result;
use cln_plugin::options::StringConfigOption;
use cln_plugin::{Builder, Plugin};
use cln_rpc::model::requests::ListpeerchannelsRequest;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify};

/// CLN option: full path for the recorder's output file.
/// When not set, defaults to `lntrace-raw.jsonl` in the CLN data directory.
const OPT_LOG: StringConfigOption = StringConfigOption::new_str_no_default(
    "lntrace-log",
    "Output path for lntrace raw JSONL (default: lntrace-raw.jsonl in CLN dir)",
);

#[derive(Clone)]
struct State {
    path: Arc<Mutex<PathBuf>>,
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

        // Open-write-close on each call so that external truncation or
        // deletion (e.g. `rm -f` on the host) doesn't leave us writing
        // to a phantom inode.
        let path = self.path.lock().await;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&*path)
            .await?;

        let mut buf = serde_json::to_string(&line)?;
        buf.push('\n');
        file.write_all(buf.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}

// One handler per notification topic. Each delegates to State::record.

/// forward_event handler with poll trigger on local_failed.
async fn on_forward_event(p: Plugin<State>, v: Value) -> Result<()> {
    let is_local_failed = v["forward_event"]["status"].as_str() == Some("local_failed");

    p.state().record("forward_event", &v).await?;

    if is_local_failed {
        p.state().poll_trigger.notify_one();
    }

    Ok(())
}

async fn on_sendpay_success(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("sendpay_success", &v).await
}

async fn on_sendpay_failure(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("sendpay_failure", &v).await
}

async fn on_pay_part_start(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("pay_part_start", &v).await
}

async fn on_pay_part_end(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("pay_part_end", &v).await
}

async fn on_channel_opened(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("channel_opened", &v).await
}

async fn on_channel_state_changed(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("channel_state_changed", &v).await
}

async fn on_invoice_payment(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("invoice_payment", &v).await
}

/// Background task that polls listpeerchannels and records the response.
///
/// Full record at startup and after triggered polls (e.g. local_failed).
/// Periodic polls only record when the response changed since last poll.
async fn poll_channels(rpc_path: PathBuf, state: State, poll_interval_secs: u64) {
    let mut rpc = match cln_rpc::ClnRpc::new(&rpc_path).await {
        Ok(rpc) => rpc,
        Err(e) => {
            eprintln!("lntrace-record: RPC connect for polling failed: {e}");
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
    let req = ListpeerchannelsRequest {
        id: None,
        channel_id: None,
        short_channel_id: None,
    };
    match rpc.call_typed(&req).await {
        Ok(resp) => {
            let payload = serde_json::to_value(&resp).unwrap_or_default();
            let current_json = serde_json::to_string(&payload).unwrap_or_default();

            // On periodic polls, skip if nothing changed.
            if !force && previous_json.as_ref() == Some(&current_json) {
                return;
            }

            previous_json.replace(current_json);

            if let Err(e) = state.record("listpeerchannels", &payload).await {
                eprintln!("lntrace-record: failed to record listpeerchannels: {e}");
            }
        }
        Err(e) => {
            eprintln!("lntrace-record: listpeerchannels poll failed: {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = State {
        path: Arc::new(Mutex::new(PathBuf::from("lntrace-raw.jsonl"))),
        node_id: Arc::new(Mutex::new("pending".to_string())),
        alias: Arc::new(Mutex::new("pending".to_string())),
        poll_trigger: Arc::new(Notify::new()),
    };

    if let Some(plugin) = Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .option(OPT_LOG)
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
        // Apply --lntrace-log option if provided.
        if let Ok(Some(log_path)) = plugin.option(&OPT_LOG) {
            let path: PathBuf = log_path.into();
            // Create parent directory if needed.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Swap in the new path (no file has been opened yet at this point).
            *plugin.state().path.lock().await = path;
        }
        // Resolve node identity via getinfo RPC before starting the
        // channel poller, so the first recorded entry already has the
        // correct node_id and alias.
        let config = plugin.configuration();
        let rpc_path = PathBuf::from(&config.lightning_dir).join(&config.rpc_file);
        {
            match cln_rpc::ClnRpc::new(&rpc_path).await {
                Ok(mut rpc) => {
                    let req = cln_rpc::model::requests::GetinfoRequest {};
                    match rpc.call_typed(&req).await {
                        Ok(info) => {
                            *plugin.state().node_id.lock().await = info.id.to_string();
                            *plugin.state().alias.lock().await = info.alias;
                        }
                        Err(e) => eprintln!("lntrace-record: getinfo failed: {e}"),
                    }
                }
                Err(e) => eprintln!("lntrace-record: RPC connect failed: {e}"),
            }
        }

        // Start channel polling (every 2 seconds).
        let poll_state = plugin.state().clone();
        tokio::spawn(poll_channels(rpc_path, poll_state, 2));

        plugin.join().await?;
    }
    Ok(())
}
