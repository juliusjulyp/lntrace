//! Shared test utilities for loading CLN fixture files.

#![allow(dead_code)]

use lntrace::*;
use lntrace_cln::translate::{translate, translate_listpeerchannels_raw};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Raw CLN plugin entry: {topic, payload, ts_ms, node_id, alias}
#[derive(serde::Deserialize)]
pub struct RawEntry {
    pub topic: String,
    pub payload: Value,
    pub ts_ms: u64,
    pub node_id: String,
    #[serde(default)]
    pub alias: String,
}

/// Read a single JSONL fixture file into raw entries.
pub fn load_fixture_file(path: &Path) -> Vec<RawEntry> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("malformed raw fixture line"))
        .collect()
}

/// Load a single fixture file from the project's `fixtures/` directory.
pub fn load_fixtures(name: &str) -> Vec<RawEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/{name}"));
    load_fixture_file(&path)
}

/// Load the 3-node baseline fixtures (node-A, node-B, node-C).
pub fn load_all_fixtures() -> Vec<RawEntry> {
    let mut all = Vec::new();
    for name in ["node-A-raw.jsonl", "node-B-raw.jsonl", "node-C-raw.jsonl"] {
        all.extend(load_fixtures(name));
    }
    all
}

/// Build envelopes from a subdirectory of `fixtures/`, including listpeerchannels snapshots.
///
/// Returns envelopes sorted by timestamp.
pub fn build_envelopes_from_dir(dir: &str, nodes: &[&str]) -> Vec<Envelope> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(dir);
    let mut envelopes = Vec::new();

    for node in nodes {
        let path = base.join(format!("node-{node}-raw.jsonl"));
        if !path.exists() {
            panic!(
                "Fixture file missing: {}. Run the lab first: cd lab && docker compose up -d --build && bash fund-and-pay.sh",
                path.display()
            );
        }
        let entries = load_fixture_file(&path);
        for (seq, entry) in entries.iter().enumerate() {
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
    envelopes
}

/// Load the 4-node reroute fixtures and return envelopes + alias map.
pub fn load_reroute() -> (Vec<Envelope>, HashMap<String, String>) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/4node-reroute");
    let nodes = ["A", "B", "C", "D"];
    let mut envelopes = Vec::new();
    let mut aliases = HashMap::new();

    for node in &nodes {
        let path = base.join(format!("node-{node}-raw.jsonl"));
        if !path.exists() {
            panic!("Fixture missing: {}", path.display());
        }
        let entries = load_fixture_file(&path);
        for (seq, entry) in entries.iter().enumerate() {
            aliases
                .entry(entry.node_id.clone())
                .or_insert_with(|| entry.alias.clone());

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
    (envelopes, aliases)
}
