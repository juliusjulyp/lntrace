use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

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
    /// Stream normalized events from a JSONL log.
    Events {
        /// Path to the JSONL event log.
        #[arg(short, long, default_value = "events.jsonl")]
        log: PathBuf,
    },

    /// Show a correlated trace for a payment hash.
    Trace {
        /// The payment hash to trace.
        payment_hash: String,

        /// Path to the JSONL event log.
        #[arg(short, long, default_value = "events.jsonl")]
        log: PathBuf,
    },

    /// Replay a recorded JSONL log through the correlator.
    Replay {
        /// Path to the JSONL log to replay.
        path: PathBuf,
    },

    /// Decode a BOLT 4 failure code.
    Explain {
        /// Failure code (decimal or 0x hex).
        code: String,
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
    }
}

async fn cmd_events(log: &Path) -> Result<()> {
    let envelopes = lntrace_collector::read_log(log).await?;
    for env in &envelopes {
        println!(
            "[{}] seq={} {}",
            env.node_id,
            env.seq,
            serde_json::to_string(&env.event)?
        );
    }
    println!("\n{} events", envelopes.len());
    Ok(())
}

async fn cmd_trace(payment_hash: &str, log: &Path) -> Result<()> {
    let envelopes = lntrace_collector::read_log(log).await?;
    let traces = lntrace_correlator::correlate(&envelopes);

    let trace = traces.iter().find(|t| t.payment_hash == payment_hash);

    match trace {
        Some(t) => {
            println!("Payment: {}", t.payment_hash);
            println!("Status:  {}", if t.success { "success" } else { "failed" });

            if t.hops.is_empty() {
                println!("\n(no hop data yet — correlator Stage 2 not implemented)");
            }

            for hop in &t.hops {
                let conf = match hop.confidence {
                    lntrace_core::Confidence::Exact => "",
                    lntrace_core::Confidence::Inferred => " [inferred]",
                    _ => " [?]",
                };
                println!(
                    "  hop {}: {} — {} msat{}",
                    hop.index, hop.node_id, hop.amount_msat, conf
                );
                if let Some(f) = &hop.failure {
                    let explanation = lntrace_explain::format_failure(f.sender_failcode);
                    println!("    FAILED: {explanation}");
                    if let Some(reason) = &f.local_failreason {
                        println!("    actual cause (from node): {reason}");
                    }
                }
            }

            if let Some(f) = &t.failure {
                let explanation = lntrace_explain::format_failure(f.failcode);
                println!("\nFailure:\n  {explanation}");
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
    let envelopes = lntrace_collector::read_log(path).await?;
    println!("Replaying {} events...", envelopes.len());

    let traces = lntrace_correlator::correlate(&envelopes);
    println!("Correlated {} traces:\n", traces.len());

    for trace in &traces {
        let status = if trace.success { "OK" } else { "FAIL" };
        println!(
            "  {} [{}] — {} hops",
            trace.payment_hash,
            status,
            trace.hops.len()
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

    println!("{}", lntrace_explain::format_failure(code));
    Ok(())
}
