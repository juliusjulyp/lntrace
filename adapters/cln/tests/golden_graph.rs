//! Golden tests for graph builder and trace list using 4-node reroute fixtures.

use lntrace::*;
use lntrace_cln::translate::{translate, translate_listpeerchannels_raw};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Raw recorder entry: {topic, payload, ts_ms, node_id, alias}
#[derive(serde::Deserialize)]
struct RawEntry {
    topic: String,
    payload: Value,
    ts_ms: u64,
    node_id: String,
    alias: String,
}

fn load_fixture_file(path: &Path) -> Vec<RawEntry> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("malformed raw fixture line"))
        .collect()
}

/// Build envelopes and aliases from a fixture directory.
fn load_reroute() -> (Vec<Envelope>, HashMap<String, String>) {
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

#[test]
fn graph_response_from_reroute() {
    let (envelopes, aliases) = load_reroute();
    let graph = build_graph(&envelopes, &aliases);

    assert_eq!(graph.nodes.len(), 4, "expected 4 nodes");
    assert_eq!(graph.channels.len(), 4, "expected 4 channels");

    // Every node should have an alias.
    for node in &graph.nodes {
        assert!(node.alias.is_some(), "node {} missing alias", node.id);
    }

    insta::assert_yaml_snapshot!("reroute_graph", graph);
}

#[test]
fn trace_list_from_reroute() {
    let (envelopes, _aliases) = load_reroute();
    let traces = correlate(&envelopes);
    let list = build_trace_list(&traces);

    assert!(
        list.len() >= 2,
        "expected at least 2 traces (drain + reroute)"
    );

    // There should be a trace with 2+ attempts (reroute) that succeeded.
    let reroute = list.iter().find(|e| e.attempt_count >= 2);
    assert!(reroute.is_some(), "expected a trace with 2+ attempts");
    assert!(reroute.unwrap().success);

    insta::assert_yaml_snapshot!("reroute_trace_list", list);
}

#[test]
fn single_trace_from_reroute() {
    let (envelopes, _aliases) = load_reroute();
    let traces = correlate(&envelopes);

    // Find the reroute trace (2+ attempts).
    let reroute_trace = traces
        .iter()
        .find(|t| t.attempts.len() >= 2)
        .expect("expected reroute trace");

    insta::assert_yaml_snapshot!("reroute_single_trace", reroute_trace);
}
