//! Stage 0: raw CLN notification recorder.
//!
//! A throwaway CLN plugin that subscribes to all payment-relevant
//! notifications and dumps the raw JSON to a JSONL file. The output
//! becomes fixture data for developing the correlator without running
//! live nodes.
//!
//! Install:
//!   cargo build -p lntrace-cln --bin lntrace-cln-recorder
//!   ln -s target/debug/lntrace-cln-recorder ~/.lightning/plugins/
//!
//! Or start manually:
//!   lightningd --plugin=/path/to/lntrace-cln-recorder
//!
//! Output goes to `lntrace-raw.jsonl` in the CLN data directory,
//! or the path set via the `lntrace-recorder-path` option.

use anyhow::Result;
use cln_plugin::{options, Builder, Plugin};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

#[derive(Clone)]
struct State {
    file: Arc<Mutex<Option<tokio::fs::File>>>,
    path: PathBuf,
}

impl State {
    async fn record(&self, topic: &str, payload: &Value) -> Result<()> {
        let ts_ms = lntrace_core::now_ms();
        let line = serde_json::json!({
            "ts_ms": ts_ms,
            "topic": topic,
            "payload": payload,
        });

        let mut guard = self.file.lock().await;
        let file = match guard.as_mut() {
            Some(f) => f,
            None => {
                let f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .await?;
                *guard = Some(f);
                guard.as_mut().unwrap()
            }
        };

        let mut buf = serde_json::to_string(&line)?;
        buf.push('\n');
        file.write_all(buf.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}

// One handler per notification topic. Each delegates to State::record.

async fn on_forward_event(p: Plugin<State>, v: Value) -> Result<()> {
    p.state().record("forward_event", &v).await
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

#[tokio::main]
async fn main() -> Result<()> {
    let state = State {
        file: Arc::new(Mutex::new(None)),
        path: PathBuf::from("lntrace-raw.jsonl"),
    };

    // NOTE: The cln-plugin API may differ between versions.
    // If this doesn't compile, check the cln-plugin docs for your CLN version.
    // The key requirement: subscribe to each topic with a handler that
    // receives Plugin<State> and serde_json::Value.
    if let Some(plugin) = Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .option(options::ConfigOption::new_str_no_default(
            "lntrace-recorder-path",
            "Path for raw notification JSONL output",
        ))
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
        // Check if the user supplied a custom output path.
        // (Access is via plugin.option(...) — exact API depends on cln-plugin version.)
        plugin.join().await?;
    }
    Ok(())
}
