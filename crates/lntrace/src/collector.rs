use crate::{now_ms, Envelope};
use anyhow::Result;
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
///
/// Reads raw bytes and only parses complete lines (terminated by `\n`).
/// Partial lines at EOF are kept in a buffer and prepended to the next read,
/// so a half-written line from a concurrent writer is never treated as malformed.
pub async fn tail_log(path: &Path, tx: mpsc::Sender<Envelope>) -> Result<()> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];

    loop {
        let n = file.read(&mut tmp).await?;
        if n == 0 {
            // No new data — wait briefly and retry (tail behaviour).
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        buf.extend_from_slice(&tmp[..n]);

        // Process every complete line (up to last \n).
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Envelope>(line) {
                Ok(env) => {
                    if tx.send(env).await.is_err() {
                        return Ok(());
                    }
                }
                Err(e) => {
                    eprintln!("warning: skipping malformed line in tail: {e}");
                }
            }
        }
        // Anything remaining in buf is a partial line — keep for next read.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::TraceEvent;
    use crate::types::NodeId;
    use std::io::Write;

    /// Build a valid envelope JSON line (terminated with \n).
    fn envelope_line(payment_hash: &str) -> String {
        let env = Envelope {
            schema_version: 1,
            node_id: NodeId("02aabb".to_string()),
            seq: 0,
            node_ts_ms: 1000,
            collector_ts_ms: None,
            event: TraceEvent::PaymentSent {
                payment_hash: payment_hash.to_string(),
                payment_preimage: None,
                amount_msat: 1000,
                fee_msat: None,
            },
        };
        serde_json::to_string(&env).unwrap()
    }

    #[tokio::test]
    async fn read_log_skips_malformed_lines() {
        let dir = std::env::temp_dir().join(format!("lntrace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("malformed.jsonl");

        let line1 = envelope_line("aaa");
        let line2 = envelope_line("bbb");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{line1}").unwrap();
            writeln!(f, "NOT VALID JSON").unwrap();
            writeln!(f, "{line2}").unwrap();
        }

        let result = read_log(&path).await.unwrap();
        assert_eq!(result.len(), 2);
        match &result[0].event {
            TraceEvent::PaymentSent { payment_hash, .. } => assert_eq!(payment_hash, "aaa"),
            other => panic!("unexpected event: {other:?}"),
        }
        match &result[1].event {
            TraceEvent::PaymentSent { payment_hash, .. } => assert_eq!(payment_hash, "bbb"),
            other => panic!("unexpected event: {other:?}"),
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn tail_buffers_partial_lines() {
        let dir = std::env::temp_dir().join(format!("lntrace-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.jsonl");

        let full_line = envelope_line("ccc");

        // Write just the first half (no newline).
        let split = full_line.len() / 2;
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&full_line.as_bytes()[..split]).unwrap();
            f.flush().unwrap();
        }

        let (tx, mut rx) = mpsc::channel::<Envelope>(16);
        let tail_path = path.clone();
        let handle = tokio::spawn(async move {
            let _ = tail_log(&tail_path, tx).await;
        });

        // Give the tailer time to read the partial line.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // No event should have arrived — the line is incomplete.
        assert!(
            rx.try_recv().is_err(),
            "partial line should not produce an event"
        );

        // Now append the rest of the line + newline.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(&full_line.as_bytes()[split..]).unwrap();
            f.write_all(b"\n").unwrap();
            f.flush().unwrap();
        }

        // The tailer should now emit exactly one event.
        let env = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("channel closed");

        match &env.event {
            TraceEvent::PaymentSent { payment_hash, .. } => assert_eq!(payment_hash, "ccc"),
            other => panic!("unexpected event: {other:?}"),
        }

        handle.abort();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
