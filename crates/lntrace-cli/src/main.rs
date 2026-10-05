mod loader;
mod server;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
        Commands::Ui { log, port } => cmd_ui(&log, port).await,
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
                let status = if attempt.success { "OK" } else { "FAIL" };
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

async fn cmd_ui(log: &Path, port: u16) -> Result<()> {
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
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Serving lntrace UI at http://localhost:{port}");

    axum::serve(listener, app).await?;
    Ok(())
}
