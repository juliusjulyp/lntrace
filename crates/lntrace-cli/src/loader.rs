//! Load envelopes from either an Envelope JSONL file or a directory of
//! raw CLN fixture files.

use anyhow::{Context, Result};
use lntrace::*;
use lntrace_cln::translate::{translate, translate_listpeerchannels_raw};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Result of loading: envelopes plus any discovered aliases.
pub struct LoadResult {
    pub envelopes: Vec<Envelope>,
    pub aliases: HashMap<String, String>,
}

/// Load from a path.
///
/// - If `path` is a directory: glob for `node-*-raw.jsonl`, translate via
///   the CLN adapter, and extract aliases from raw entries.
/// - If `path` is a file: read as Envelope JSONL (aliases will be empty).
pub async fn load(path: &Path) -> Result<LoadResult> {
    if path.is_dir() {
        load_directory(path).await
    } else {
        let envelopes = read_log(path).await?;
        Ok(LoadResult {
            envelopes,
            aliases: HashMap::new(),
        })
    }
}

/// Raw recorder entry from CLN fixture files.
#[derive(serde::Deserialize)]
struct RawEntry {
    topic: String,
    payload: Value,
    ts_ms: u64,
    node_id: String,
    #[serde(default)]
    alias: String,
}

async fn load_directory(dir: &Path) -> Result<LoadResult> {
    let mut envelopes = Vec::new();
    let mut aliases = HashMap::new();

    // Find all node-*-raw.jsonl files.
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .map_or(false, |f| f.starts_with("node-") && f.ends_with("-raw.jsonl"))
        })
        .collect();
    paths.sort();

    if paths.is_empty() {
        anyhow::bail!("no node-*-raw.jsonl files found in {}", dir.display());
    }

    for path in &paths {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;

        for (seq, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: RawEntry = serde_json::from_str(line)
                .with_context(|| format!("parsing line {} of {}", seq + 1, path.display()))?;

            // Record alias.
            if !entry.alias.is_empty() {
                aliases
                    .entry(entry.node_id.clone())
                    .or_insert_with(|| entry.alias.clone());
            }

            // Translate.
            if entry.topic == "listpeerchannels" {
                let channels = translate_listpeerchannels_raw(&entry.payload);
                if !channels.is_empty() {
                    envelopes.push(Envelope {
                        schema_version: SCHEMA_VERSION,
                        node_id: NodeId(entry.node_id.clone()),
                        seq: seq as u64,
                        node_ts_ms: entry.ts_ms,
                        collector_ts_ms: Some(entry.ts_ms + 1),
                        event: TraceEvent::Snapshot { channels },
                    });
                }
            } else if let Some(event) = translate(&entry.topic, &entry.payload) {
                envelopes.push(Envelope {
                    schema_version: SCHEMA_VERSION,
                    node_id: NodeId(entry.node_id.clone()),
                    seq: seq as u64,
                    node_ts_ms: entry.ts_ms,
                    collector_ts_ms: Some(entry.ts_ms + 1),
                    event,
                });
            }
        }
    }

    envelopes.sort_by_key(|e| e.node_ts_ms);
    Ok(LoadResult { envelopes, aliases })
}
