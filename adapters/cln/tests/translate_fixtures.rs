//! Integration tests for CLN notification → TraceEvent translation.
//!
//! Reads the raw recorder fixtures from `fixtures/cln-regtest-raw.jsonl`,
//! translates each notification, and verifies the output.
//!
//! Also generates `fixtures/cln-regtest-envelope.jsonl` in Envelope format
//! so that the CLI `events`, `trace`, and `replay` commands work out of the box.

use lntrace_cln::translate::translate;
use lntrace_core::*;
use serde_json::Value;
use std::path::Path;

/// Raw recorder entry: {topic, payload, ts_ms}
#[derive(serde::Deserialize)]
struct RawEntry {
    topic: String,
    payload: Value,
    ts_ms: u64,
}

fn load_raw_fixtures() -> Vec<RawEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cln-regtest-raw.jsonl");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("malformed raw fixture line"))
        .collect()
}

#[test]
fn all_raw_fixtures_translate() {
    let entries = load_raw_fixtures();
    assert!(!entries.is_empty(), "no raw fixtures found");

    let mut translated = 0;
    let mut skipped = 0;

    for entry in &entries {
        match translate(&entry.topic, &entry.payload) {
            Some(_event) => translated += 1,
            None => skipped += 1,
        }
    }

    // We expect most events to translate (channel_state_changed for
    // non-close states are skipped).
    assert!(translated > 0, "no events were translated");
    eprintln!("translated: {translated}, skipped: {skipped}");
}

#[test]
fn sendpay_success_translates_to_payment_sent() {
    let entries = load_raw_fixtures();
    let entry = entries
        .iter()
        .find(|e| e.topic == "sendpay_success")
        .expect("no sendpay_success in fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("sendpay_success should translate");

    match &event {
        TraceEvent::PaymentSent {
            payment_hash,
            payment_preimage,
            amount_msat,
            ..
        } => {
            assert!(!payment_hash.is_empty());
            assert!(payment_preimage.is_some());
            assert!(*amount_msat > 0);
        }
        other => panic!("expected PaymentSent, got {other:?}"),
    }
}

#[test]
fn sendpay_failure_translates_to_payment_failed() {
    let entries = load_raw_fixtures();
    let entry = entries
        .iter()
        .find(|e| e.topic == "sendpay_failure")
        .expect("no sendpay_failure in fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("sendpay_failure should translate");

    match &event {
        TraceEvent::PaymentFailed {
            payment_hash,
            failcode,
            reason,
        } => {
            assert!(!payment_hash.is_empty());
            assert!(failcode.is_some(), "failcode should be present");
            assert_eq!(failcode.unwrap(), 16399); // WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS
            assert!(reason.is_some());
        }
        other => panic!("expected PaymentFailed, got {other:?}"),
    }
}

#[test]
fn channel_state_changed_skips_non_close() {
    let entries = load_raw_fixtures();
    let entry = entries
        .iter()
        .find(|e| e.topic == "channel_state_changed")
        .expect("no channel_state_changed in fixtures");

    // Our fixtures have CHANNELD_AWAITING_LOCKIN and CHANNELD_NORMAL,
    // neither of which is ONCHAIN/CLOSED, so translate should return None.
    let event = translate(&entry.topic, &entry.payload);
    assert!(
        event.is_none(),
        "non-close channel_state_changed should be skipped"
    );
}

#[test]
fn envelope_serde_round_trip() {
    let entries = load_raw_fixtures();
    let node_id = NodeId("02test".to_string());

    for (seq, entry) in entries.iter().enumerate() {
        if let Some(event) = translate(&entry.topic, &entry.payload) {
            let envelope = Envelope {
                schema_version: SCHEMA_VERSION,
                node_id: node_id.clone(),
                seq: seq as u64,
                node_ts_ms: entry.ts_ms,
                collector_ts_ms: Some(entry.ts_ms + 1),
                event,
            };

            let json = serde_json::to_string(&envelope).expect("serialize");
            let back: Envelope = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back.schema_version, SCHEMA_VERSION);
            assert_eq!(back.seq, seq as u64);
            assert_eq!(back.node_ts_ms, entry.ts_ms);
        }
    }
}

#[test]
fn generate_envelope_fixture() {
    let entries = load_raw_fixtures();
    let node_id =
        NodeId("025af3193e21e10a6d9603995760b73ae265d63129c9230bb70fd882f5b1392aae".to_string());
    let out_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cln-regtest-envelope.jsonl");

    let mut lines = Vec::new();
    let mut seq = 0u64;

    for entry in &entries {
        if let Some(event) = translate(&entry.topic, &entry.payload) {
            let envelope = Envelope {
                schema_version: SCHEMA_VERSION,
                node_id: node_id.clone(),
                seq,
                node_ts_ms: entry.ts_ms,
                collector_ts_ms: Some(entry.ts_ms + 1),
                event,
            };
            lines.push(serde_json::to_string(&envelope).expect("serialize"));
            seq += 1;
        }
    }

    assert!(!lines.is_empty(), "no envelopes generated");
    let content = lines.join("\n") + "\n";
    std::fs::write(&out_path, &content)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));

    eprintln!("wrote {} envelopes to {}", lines.len(), out_path.display());
}
