mod live;
mod loader;
mod server;
mod tailer;
mod ws;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Parser)]
#[command(
    name = "lntrace",
    about = "Payment debugging for the Lightning Network"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Stream normalized events from a JSONL log or raw fixture directory.
    Events {
        /// Path to JSONL log file or directory of raw fixture files.
        #[arg(short, long, default_value = "events.jsonl")]
        log: PathBuf,
    },

    /// Show a correlated trace for a payment hash.
    Trace {
        /// The payment hash to trace.
        payment_hash: String,

        /// Path to JSONL log file or directory of raw fixture files.
        #[arg(short, long, default_value = "events.jsonl")]
        log: PathBuf,
    },

    /// Replay a recorded JSONL log or raw fixture directory through the correlator.
    Replay {
        /// Path to JSONL log file or directory of raw fixture files.
        path: PathBuf,
    },

    /// Decode a BOLT 4 failure code.
    Explain {
        /// Failure code (decimal or 0x hex).
        code: String,
    },

    /// Launch the trace graph UI.
    Ui {
        /// Path to JSONL log file or directory of raw fixture files.
        #[arg(short, long)]
        log: PathBuf,

        /// Port to serve on.
        #[arg(short, long, default_value = "8080")]
        port: u16,

        /// Watch the log directory for new events and push live updates.
        #[arg(short, long)]
        follow: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Events { log } => cmd_events(&log).await,
        Commands::Trace { payment_hash, log } => cmd_trace(&payment_hash, &log).await,
        Commands::Replay { path } => cmd_replay(&path).await,
        Commands::Explain { code } => cmd_explain(&code),
        Commands::Ui { log, port, follow } => cmd_ui(&log, port, follow).await,
    }
}

async fn cmd_events(log: &Path) -> Result<()> {
    let result = loader::load(log).await?;
    for env in &result.envelopes {
        let alias = result.aliases.get(env.node_id.0.as_str());
        let label = alias.map_or_else(
            || format!("[{}...]", &env.node_id.0[..12]),
            |a| format!("[{a}]"),
        );
        println!(
            "{label} seq={} {}",
            env.seq,
            serde_json::to_string(&env.event)?
        );
    }
    println!("\n{} events", result.envelopes.len());
    Ok(())
}

async fn cmd_trace(payment_hash: &str, log: &Path) -> Result<()> {
    let result = loader::load(log).await?;
    let traces = lntrace::correlate(&result.envelopes);

    let trace = traces.iter().find(|t| t.payment_hash == payment_hash);

    match trace {
        Some(t) => {
            println!("Payment: {}", t.payment_hash);
            println!("Status:  {}", if t.success { "success" } else { "failed" });
            println!("Attempts: {}", t.attempts.len());

            for (ai, attempt) in t.attempts.iter().enumerate() {
                let status = match attempt.outcome {
                    lntrace::AttemptOutcome::Succeeded => "OK",
                    lntrace::AttemptOutcome::Failed => "FAIL",
                    lntrace::AttemptOutcome::Cancelled => "CANCELLED",
                    lntrace::AttemptOutcome::InFlight => "IN_FLIGHT",
                };
                let group = attempt
                    .groupid
                    .map(|g| format!(" group={g}"))
                    .unwrap_or_default();
                let part = attempt
                    .partid
                    .map(|p| format!(" part={p}"))
                    .unwrap_or_default();
                let dur = attempt
                    .duration_secs
                    .map(|d| format!(" ({:.3}s)", d))
                    .unwrap_or_default();
                println!("\n  Attempt {} [{status}]{group}{part}{dur}", ai + 1);

                for hop in &attempt.hops {
                    let conf = match hop.confidence {
                        lntrace::Confidence::Exact => "",
                        lntrace::Confidence::Inferred => " [inferred]",
                        _ => " [?]",
                    };
                    println!(
                        "    hop {}: {} — {} msat{}",
                        hop.index, hop.node_id, hop.amount_msat, conf
                    );
                    if let Some(f) = &hop.failure {
                        let explanation = lntrace::format_failure(f.sender_failcode);
                        println!("      FAILED: {explanation}");
                        if let Some(reason) = &f.local_failreason {
                            println!("      from node: {reason}");
                        }
                        if let Some(cause) = &f.inferred_cause {
                            println!("      inferred cause: {cause}");
                        }
                    }
                }

                if let Some(f) = &attempt.failure {
                    let explanation = lntrace::format_failure(f.failcode);
                    println!("    {explanation}");
                }
            }

            if let Some(f) = &t.failure {
                let explanation = lntrace::format_failure(f.failcode);
                println!("\nOverall failure:\n  {explanation}");
            }
        }
        None => {
            println!("No trace found for payment hash: {payment_hash}");
            println!("Available hashes:");
            for t in &traces {
                println!("  {}", t.payment_hash);
            }
        }
    }
    Ok(())
}

async fn cmd_replay(path: &Path) -> Result<()> {
    let result = loader::load(path).await?;
    println!("Replaying {} events...", result.envelopes.len());

    let traces = lntrace::correlate(&result.envelopes);
    println!("Correlated {} traces:\n", traces.len());

    for trace in &traces {
        let status = if trace.success { "OK" } else { "FAIL" };
        let amt = trace
            .attempts
            .first()
            .map_or(0, |a| a.hops.last().map_or(0, |h| h.amount_msat / 1000));
        println!(
            "  {} [{status}] — {amt} sat — {} attempt(s)",
            trace.payment_hash,
            trace.attempts.len()
        );
    }
    Ok(())
}

fn cmd_explain(code_str: &str) -> Result<()> {
    let code: u16 = if let Some(hex) = code_str.strip_prefix("0x") {
        u16::from_str_radix(hex, 16)?
    } else {
        code_str.parse()?
    };

    println!("{}", lntrace::format_failure(code));
    Ok(())
}

async fn cmd_ui(log: &Path, port: u16, follow: bool) -> Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    if follow {
        if !log.is_dir() {
            anyhow::bail!("--follow requires a directory path, not a file");
        }

        // In follow mode, start with whatever is already on disk (possibly nothing).
        let result = match loader::load(log).await {
            Ok(r) => r,
            Err(_) => loader::LoadResult {
                envelopes: Vec::new(),
                aliases: std::collections::HashMap::new(),
            },
        };
        let traces = lntrace::correlate(&result.envelopes);
        let graph = lntrace::build_graph(&result.envelopes, &result.aliases);
        let trace_list = lntrace::build_trace_list(&traces);

        println!(
            "Loaded {} events, {} traces, {} nodes, {} channels",
            result.envelopes.len(),
            traces.len(),
            graph.nodes.len(),
            graph.channels.len()
        );

        let live_state = Arc::new(RwLock::new(live::LiveState {
            envelopes: result.envelopes,
            aliases: result.aliases,
            graph,
            traces,
            trace_list,
        }));

        let (tail_tx, tail_rx) = tokio::sync::mpsc::channel(256);
        let (update_tx, _) = tokio::sync::broadcast::channel(64);

        // Compute initial file offsets so the tailer skips already-loaded data.
        let initial_offsets: std::collections::HashMap<std::path::PathBuf, u64> =
            std::fs::read_dir(log)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|n| n.ends_with(".jsonl"))
                })
                .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())))
                .collect();

        // Spawn directory tailer.
        let dir = log.to_path_buf();
        tokio::spawn(async move {
            if let Err(e) = tailer::tail_directory(&dir, 250, tail_tx, initial_offsets).await {
                eprintln!("tailer error: {e}");
            }
        });

        // Spawn state manager.
        let sm_state = live_state.clone();
        let sm_tx = update_tx.clone();
        tokio::spawn(async move {
            live::run_state_manager(sm_state, tail_rx, sm_tx, 200).await;
        });

        let app_state = ws::LiveAppState {
            live: live_state,
            update_tx,
        };
        let app = server::build_live_router(app_state);

        println!("Serving lntrace UI at http://localhost:{port} (live)");
        axum::serve(listener, app).await?;
    } else {
        let result = loader::load(log).await?;
        let traces = lntrace::correlate(&result.envelopes);
        let graph = lntrace::build_graph(&result.envelopes, &result.aliases);
        let trace_list = lntrace::build_trace_list(&traces);

        println!(
            "Loaded {} events, {} traces, {} nodes, {} channels",
            result.envelopes.len(),
            traces.len(),
            graph.nodes.len(),
            graph.channels.len()
        );

        let state = Arc::new(server::AppState {
            graph,
            traces,
            trace_list,
        });

        let app = server::build_router(state);
        println!("Serving lntrace UI at http://localhost:{port}");
        axum::serve(listener, app).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // First line of 4node-success/node-A-raw.jsonl: pay_part_start.
    const FIXTURE_LINE_1: &str = r#"{"alias":"node-A","node_id":"034b843630914ddc28d1c97d5cb3afe9bbdc39a986c60d70644b0e0a627fbfceb4","payload":{"pay_part_start":{"attempt_msat":50000000,"groupid":7084061114156922794,"hops":[{"channel_in_msat":50000501,"channel_out_msat":50000501,"direction":0,"next_node":"03a679f6d264f054ca24cc469ec2939cc5c03965a556ebf8047b96d282e4ca9724","short_channel_id":"108x1x0"},{"channel_in_msat":50000501,"channel_out_msat":50000000,"direction":1,"next_node":"02b7629c77973b95e679f456d852412f0ecd97f1ee1ff8b4e959d05b2f8f1defde","short_channel_id":"108x2x0"}],"partid":1,"payment_hash":"4c47ded7ad81b259bf657ccaf5dc64aa558083eec60995694d05e6baacd7120b","total_payment_msat":50000000}},"topic":"pay_part_start","ts_ms":1790981619628}"#;

    // Second line: sendpay_success (completes the payment).
    const FIXTURE_LINE_2: &str = r#"{"alias":"node-A","node_id":"034b843630914ddc28d1c97d5cb3afe9bbdc39a986c60d70644b0e0a627fbfceb4","payload":{"sendpay_success":{"amount_msat":50000000,"amount_sent_msat":50000501,"bolt11":"lnbcrt500u1p4vqd0nsp5jv04e43kglc8kazl204k50x372skzagmp3qjeg466ulvzfwajcespp5f3raa4adsxe9n0m90n90thry4f2cpqlwccye262dqhnt4txhzg9sdq4x3hx7er9ypeh2cmrv4ehxxqyjw5qcqp2rzjqwn8nakjvnc9fj3ye3rfas5nnnzuqwt954twh7qy0wtd9qhye2tjgqqqdsqqqqsqqqqqqqqpqqqqqzsqqc9qxpqysgqzncsla28sgl6d4fh43h89uf0mlem7hguaqzt72vruapk96pr3nkywup2q20hnswuu6v4jyphusclcywmzpdrsnur0z2rsfw8r5mjjggqnjxj8y","completed_at":1790981620,"created_at":1790981619,"created_index":1,"destination":"02b7629c77973b95e679f456d852412f0ecd97f1ee1ff8b4e959d05b2f8f1defde","groupid":7084061114156922794,"id":1,"partid":1,"payment_hash":"4c47ded7ad81b259bf657ccaf5dc64aa558083eec60995694d05e6baacd7120b","payment_preimage":"b279e0c3d6f35f22c6c5f399749bc17ac94fcc66e74f491859f5a0bd485a7ffc","status":"complete","updated_index":1}},"topic":"sendpay_success","ts_ms":1790981620117}"#;

    #[tokio::test]
    async fn live_mode_pushes_updates_via_ws() {
        use futures::stream::StreamExt;
        use tokio_tungstenite::connect_async;

        let dir = std::env::temp_dir().join(format!("lntrace-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture_path = dir.join("node-A-raw.jsonl");

        // Write initial fixture line.
        {
            let mut f = std::fs::File::create(&fixture_path).unwrap();
            writeln!(f, "{FIXTURE_LINE_1}").unwrap();
        }

        // Load initial state.
        let result = loader::load(&dir).await.unwrap();
        let traces = lntrace::correlate(&result.envelopes);
        let graph = lntrace::build_graph(&result.envelopes, &result.aliases);
        let trace_list = lntrace::build_trace_list(&traces);

        let initial_trace_count = trace_list.len();

        let live_state = Arc::new(RwLock::new(live::LiveState {
            envelopes: result.envelopes,
            aliases: result.aliases,
            graph,
            traces,
            trace_list,
        }));

        let (tail_tx, tail_rx) = tokio::sync::mpsc::channel(256);
        let (update_tx, _) = tokio::sync::broadcast::channel(64);

        // Spawn tailer (with initial offsets so it skips already-loaded data).
        let tail_dir = dir.clone();
        let initial_offsets: std::collections::HashMap<std::path::PathBuf, u64> =
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|n| n.ends_with(".jsonl"))
                })
                .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())))
                .collect();
        tokio::spawn(async move {
            let _ = tailer::tail_directory(&tail_dir, 100, tail_tx, initial_offsets).await;
        });

        // Spawn state manager.
        let sm_state = live_state.clone();
        let sm_tx = update_tx.clone();
        tokio::spawn(async move {
            live::run_state_manager(sm_state, tail_rx, sm_tx, 100).await;
        });

        let app_state = ws::LiveAppState {
            live: live_state,
            update_tx,
        };
        let app = server::build_live_router(app_state);

        // Bind to port 0 (OS picks a free port).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect WebSocket.
        let url = format!("ws://127.0.0.1:{port}/ws");
        let (mut ws_stream, _) = connect_async(&url).await.expect("WS connect failed");

        // Receive snapshot.
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws_stream.next())
            .await
            .expect("timed out waiting for snapshot")
            .expect("stream ended")
            .expect("ws error");

        let snapshot: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(
            snapshot["traces"].as_array().unwrap().len(),
            initial_trace_count
        );

        // Append second fixture line (completes the payment).
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&fixture_path)
                .unwrap();
            writeln!(f, "{FIXTURE_LINE_2}").unwrap();
        }

        // Receive update within 5 seconds (CI machines are slower).
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws_stream.next())
            .await
            .expect("timed out waiting for update")
            .expect("stream ended")
            .expect("ws error");

        let update: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
        assert_eq!(update["type"], "update");
        assert!(!update["changed"].as_array().unwrap().is_empty());

        // The payment hash should be in the changed list.
        let changed = update["changed"].as_array().unwrap();
        let expected_hash = "4c47ded7ad81b259bf657ccaf5dc64aa558083eec60995694d05e6baacd7120b";
        assert!(
            changed.iter().any(|h| h.as_str() == Some(expected_hash)),
            "expected hash not in changed: {changed:?}"
        );

        // Cleanup.
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
