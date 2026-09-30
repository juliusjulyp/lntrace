use anyhow::Result;
use lntrace_core::{now_ms, Envelope};
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Writes envelopes to an append-only JSONL log.
pub struct LogWriter {
    path: PathBuf,
    rx: mpsc::Receiver<Envelope>,
}

/// Handle for sending envelopes to the log writer.
#[derive(Clone)]
pub struct LogHandle {
    tx: mpsc::Sender<Envelope>,
}

impl LogHandle {
    /// Ingest an envelope: stamp collector time and send to writer.
    pub async fn ingest(&self, mut envelope: Envelope) -> Result<()> {
        envelope.collector_ts_ms = Some(now_ms());
        self.tx.send(envelope).await?;
        Ok(())
    }
}

impl LogWriter {
    /// Create a new log writer and its ingest handle.
    pub fn new(path: impl Into<PathBuf>) -> (Self, LogHandle) {
        let (tx, rx) = mpsc::channel(4096);
        (
            LogWriter {
                path: path.into(),
                rx,
            },
            LogHandle { tx },
        )
    }

    /// Run the writer loop. Blocks until all handles are dropped.
    pub async fn run(mut self) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        while let Some(envelope) = self.rx.recv().await {
            let mut line = serde_json::to_string(&envelope)?;
            line.push('\n');
            file.write_all(line.as_bytes()).await?;
            file.flush().await?;
        }
        Ok(())
    }
}

/// Read envelopes from an existing JSONL log file.
pub async fn read_log(path: &Path) -> Result<Vec<Envelope>> {
    let file = tokio::fs::File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut envelopes = Vec::new();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Envelope>(&line) {
            Ok(env) => envelopes.push(env),
            Err(e) => eprintln!("warning: skipping malformed log line: {e}"),
        }
    }
    Ok(envelopes)
}

/// Stream envelopes from a JSONL log file, tailing for new entries.
pub async fn tail_log(path: &Path, tx: mpsc::Sender<Envelope>) -> Result<()> {
    let file = tokio::fs::File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    loop {
        match lines.next_line().await? {
            Some(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if let Ok(env) = serde_json::from_str::<Envelope>(&line) {
                    if tx.send(env).await.is_err() {
                        break;
                    }
                }
            }
            None => {
                // No more lines yet — wait briefly and retry (tail behaviour).
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    Ok(())
}
