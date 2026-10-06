//! End-to-end golden test: raw CLN fixtures → translate → envelopes → correlate → snapshot.
//!
//! Uses `insta` for YAML snapshot comparison. Run `cargo insta review` to approve changes.

use lntrace::*;
use lntrace_cln::translate::{translate, translate_listpeerchannels_raw};
use serde_json::Value;
use std::path::Path;

/// Raw recorder entry: {topic, payload, ts_ms, node_id, alias}
#[derive(serde::Deserialize)]
struct RawEntry {
    topic: String,
    payload: Value,
    ts_ms: u64,
    node_id: String,
    #[allow(dead_code)]
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

fn load_fixtures(name: &str) -> Vec<RawEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/{name}"));
    load_fixture_file(&path)
}

/// Build envelopes from all three node fixture files (3-node regression).
fn build_envelopes() -> Vec<Envelope> {
    let mut envelopes = Vec::new();

    for (file, alias) in [
        ("node-A-raw.jsonl", "A"),
        ("node-B-raw.jsonl", "B"),
        ("node-C-raw.jsonl", "C"),
    ] {
        let entries = load_fixtures(file);
        for (seq, entry) in entries.iter().enumerate() {
            if let Some(event) = translate(&entry.topic, &entry.payload) {
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
        let _ = alias; // used for context, not in envelope
    }

    // Sort by timestamp for deterministic ordering.
    envelopes.sort_by_key(|e| e.node_ts_ms);
    envelopes
}

/// Build envelopes from a subdirectory of fixtures/, including listpeerchannels snapshots.
fn build_envelopes_from_dir(dir: &str, nodes: &[&str]) -> Vec<Envelope> {
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

// --- 3-node regression test (unchanged) ---

#[test]
fn golden_correlator_traces() {
    let envelopes = build_envelopes();
    assert!(!envelopes.is_empty(), "no envelopes built from fixtures");

    let traces = correlate(&envelopes);
    assert!(!traces.is_empty(), "no traces produced");

    insta::assert_yaml_snapshot!("correlator_traces", traces);
}

// --- 4-node golden tests ---

const NODES_4: &[&str] = &["A", "B", "C", "D"];

#[test]
fn golden_4node_success() {
    let envelopes = build_envelopes_from_dir("4node-success", NODES_4);
    assert!(!envelopes.is_empty(), "no envelopes from 4node-success");

    let traces = correlate(&envelopes);
    assert!(!traces.is_empty(), "no traces produced");

    // Should contain a single successful payment via B.
    let main_trace = traces
        .iter()
        .find(|t| t.success)
        .expect("expected a successful trace");
    assert_eq!(
        main_trace.attempts.len(),
        1,
        "success should have exactly 1 attempt"
    );
    assert_eq!(main_trace.attempts[0].outcome, AttemptOutcome::Succeeded);

    insta::assert_yaml_snapshot!("4node_success_traces", traces);
}

#[test]
fn golden_4node_reroute() {
    let envelopes = build_envelopes_from_dir("4node-reroute", NODES_4);
    assert!(!envelopes.is_empty(), "no envelopes from 4node-reroute");

    let traces = correlate(&envelopes);

    // Find the reroute trace: the one with 2 attempts (not the drain payment).
    let reroute_trace = traces
        .iter()
        .find(|t| t.attempts.len() >= 2)
        .expect("expected a trace with 2+ attempts (reroute)");

    // Overall payment succeeded (retry via D worked).
    assert!(reroute_trace.success);

    // Attempt 0: failed via B with 0x1007.
    let a0 = &reroute_trace.attempts[0];
    assert_eq!(a0.outcome, AttemptOutcome::Failed);
    assert_eq!(a0.failure.as_ref().unwrap().failcode, 4103);

    // Attempt 0: failing hop should have inferred_cause from snapshot.
    let failing_hop = a0
        .hops
        .iter()
        .find(|h| h.failure.is_some())
        .expect("expected a failing hop in attempt 0");
    let hop_fail = failing_hop.failure.as_ref().unwrap();
    assert_eq!(hop_fail.sender_failcode, 4103);
    assert!(
        hop_fail
            .inferred_cause
            .as_ref()
            .expect("expected inferred_cause on failing hop")
            .contains("insufficient outbound liquidity"),
        "inferred_cause should mention insufficient outbound liquidity, got: {:?}",
        hop_fail.inferred_cause
    );

    // Attempt 1: succeeded via D.
    let a1 = &reroute_trace.attempts[1];
    assert_eq!(a1.outcome, AttemptOutcome::Succeeded);
    // xpay retried — either different groupid or different partid.
    assert!(
        a0.groupid != a1.groupid || a0.partid != a1.partid,
        "retry should have different groupid or partid"
    );

    // --- Direction invariant: route hop ↔ B's listpeerchannels snapshot ---
    // The enrichment join is: route hop's (scid, direction) must match
    // the snapshot's (scid, direction) for the same channel at B.
    //
    // We go back to the raw envelopes to get the route hop data (TracedHop
    // doesn't carry scid/direction) and compare with B's snapshot.
    let erring_node = a0.failure.as_ref().unwrap().erring_node.as_ref().unwrap();

    // Find the PaymentPathAttempt for this payment's failing attempt.
    let attempt_env = envelopes
        .iter()
        .find(|env| {
            if let TraceEvent::PaymentPathAttempt {
                payment_hash,
                groupid,
                ..
            } = &env.event
            {
                payment_hash == &reroute_trace.payment_hash && *groupid == a0.groupid
            } else {
                false
            }
        })
        .expect("expected PaymentPathAttempt envelope for failing attempt");

    // Extract the route hop for the failing channel (hop at failing_hop.index).
    let route_hop = match &attempt_env.event {
        TraceEvent::PaymentPathAttempt { route, .. } => &route[failing_hop.index],
        _ => unreachable!(),
    };
    let route_direction = route_hop.direction;
    let route_scid = route_hop.channel.scid.expect("route hop should have scid");

    // Find B's snapshot and look up the same scid.
    let b_snapshot_env = envelopes
        .iter()
        .find(|env| {
            env.node_id == *erring_node && matches!(&env.event, TraceEvent::Snapshot { .. })
        })
        .expect("expected a Snapshot envelope from B");

    let snap_ch = match &b_snapshot_env.event {
        TraceEvent::Snapshot { channels } => channels
            .iter()
            .find(|ch| ch.channel.scid == Some(route_scid))
            .expect("B's snapshot should contain the failing channel"),
        _ => unreachable!(),
    };

    // The invariant: route hop direction == snapshot direction.
    assert_eq!(
        route_direction, snap_ch.direction,
        "route hop direction ({:?}) must equal B's snapshot direction ({:?}) for scid {}",
        route_direction, snap_ch.direction, route_scid
    );

    insta::assert_yaml_snapshot!("4node_reroute_traces", traces);
}

#[test]
fn golden_4node_allfail() {
    let envelopes = build_envelopes_from_dir("4node-allfail", NODES_4);
    assert!(!envelopes.is_empty(), "no envelopes from 4node-allfail");

    let traces = correlate(&envelopes);

    // Find the trace where all attempts failed (not a drain payment).
    let fail_trace = traces
        .iter()
        .find(|t| !t.success && t.attempts.len() >= 2)
        .expect("expected a trace with multiple failed attempts");

    assert!(!fail_trace.success);
    assert!(fail_trace.failure.is_some());

    // 4 failed + 1 cancelled = 5 attempts.
    assert_eq!(fail_trace.attempts.len(), 5);
    let failed = fail_trace
        .attempts
        .iter()
        .filter(|a| a.outcome == AttemptOutcome::Failed)
        .count();
    let cancelled = fail_trace
        .attempts
        .iter()
        .filter(|a| a.outcome == AttemptOutcome::Cancelled)
        .count();
    assert_eq!(failed, 4);
    assert_eq!(cancelled, 1);

    // Partid 2 is the cancelled one (start with no end).
    let p2 = fail_trace
        .attempts
        .iter()
        .find(|a| a.partid == Some(2))
        .expect("expected partid 2");
    assert_eq!(p2.outcome, AttemptOutcome::Cancelled);
    assert!(p2.failure.is_none());
    assert!(p2.duration_secs.is_none());

    insta::assert_yaml_snapshot!("4node_allfail_traces", traces);
}
