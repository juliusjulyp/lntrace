//! Directory tailer: polls for `*.jsonl` files and streams new
//! envelopes as they are appended.

use crate::loader::{is_raw_jsonl, translate_entry, RawEntry};
use anyhow::Result;
use lntrace::Envelope;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

/// A batch of new data produced by one poll cycle.
pub struct TailBatch {
    pub envelopes: Vec<Envelope>,
    pub aliases: HashMap<String, String>,
}

/// Per-file read state.
struct FileState {
    offset: u64,
    buf: Vec<u8>,
    seq: u64,
}

/// Poll `dir` every `poll_ms` milliseconds for `*.jsonl` files.
///
/// New lines are parsed as [`RawEntry`] and translated to [`Envelope`]s via the
/// CLN adapter. Partial lines at EOF are buffered until the next poll.
/// If a file shrinks (truncated or recreated), the offset resets to zero.
///
/// `initial_offsets` lets the caller skip already-loaded data. Pass each
/// file's byte size after the initial `loader::load()` so the tailer only
/// picks up new lines.
pub async fn tail_directory(
    dir: &Path,
    poll_ms: u64,
    tx: mpsc::Sender<TailBatch>,
    initial_offsets: HashMap<PathBuf, u64>,
) -> Result<()> {
    let mut files: HashMap<PathBuf, FileState> = initial_offsets
        .into_iter()
        .map(|(path, offset)| {
            (
                path,
                FileState {
                    offset,
                    buf: Vec::new(),
                    seq: 0,
                },
            )
        })
        .collect();

    loop {
        let mut batch_envelopes = Vec::new();
        let mut batch_aliases = HashMap::new();

        // Discover files.
        let entries = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                eprintln!("warning: cannot read directory {}: {e}", dir.display());
                tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                continue;
            }
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = match path.file_name().and_then(|f| f.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !is_raw_jsonl(&name) {
                continue;
            }

            let state = files.entry(path.clone()).or_insert(FileState {
                offset: 0,
                buf: Vec::new(),
                seq: 0,
            });

            // Check file size for truncation.
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() < state.offset {
                // File was truncated or recreated — reset.
                state.offset = 0;
                state.buf.clear();
                state.seq = 0;
            }
            if meta.len() == state.offset && state.buf.is_empty() {
                // No new data.
                continue;
            }

            // Read new bytes from offset.
            let mut file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if state.offset > 0 && file.seek(SeekFrom::Start(state.offset)).is_err() {
                continue;
            }
            let mut new_bytes = Vec::new();
            if file.read_to_end(&mut new_bytes).is_err() {
                continue;
            }
            state.offset += new_bytes.len() as u64;
            state.buf.extend_from_slice(&new_bytes);

            // Process complete lines.
            while let Some(pos) = state.buf.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = state.buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line_bytes);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let entry: RawEntry = match serde_json::from_str(line) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!(
                            "warning: skipping malformed line in {}: {e}",
                            path.display()
                        );
                        continue;
                    }
                };

                if !entry.alias.is_empty() {
                    batch_aliases
                        .entry(entry.node_id.clone())
                        .or_insert_with(|| entry.alias.clone());
                }

                if let Some(envelope) = translate_entry(&entry, state.seq) {
                    batch_envelopes.push(envelope);
                }
                state.seq += 1;
            }
        }

        if !batch_envelopes.is_empty() {
            let batch = TailBatch {
                envelopes: batch_envelopes,
                aliases: batch_aliases,
            };
            if tx.send(batch).await.is_err() {
                break;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
    }

    Ok(())
}
