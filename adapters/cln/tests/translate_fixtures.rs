//! Integration tests for CLN notification -> TraceEvent translation.
//!
//! Reads raw recorder fixtures from `fixtures/node-{A,B,C}-raw.jsonl`,
//! translates each notification, and verifies the output against known values.

use lntrace::*;
use lntrace_cln::translate::translate;
use serde_json::Value;
use std::path::Path;

/// Raw recorder entry: {topic, payload, ts_ms, node_id, alias}
#[derive(serde::Deserialize)]
struct RawEntry {
    topic: String,
    payload: Value,
    ts_ms: u64,
    #[allow(dead_code)]
    node_id: String,
    #[allow(dead_code)]
    alias: String,
}

fn load_fixtures(name: &str) -> Vec<RawEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/{name}"));
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("malformed raw fixture line"))
        .collect()
}

fn load_all_fixtures() -> Vec<RawEntry> {
    let mut all = Vec::new();
    for name in ["node-A-raw.jsonl", "node-B-raw.jsonl", "node-C-raw.jsonl"] {
        all.extend(load_fixtures(name));
    }
    all
}

// --- Bulk translation ---

#[test]
fn all_fixtures_translate() {
    let entries = load_all_fixtures();
    assert!(!entries.is_empty(), "no raw fixtures found");

    let mut translated = 0;
    let mut skipped = 0;

    for entry in &entries {
        match translate(&entry.topic, &entry.payload) {
            Some(_) => translated += 1,
            None => skipped += 1,
        }
    }

    assert!(translated > 0, "no events were translated");
    // channel_state_changed for non-close states are skipped
    assert!(skipped > 0, "expected some skipped events");
    eprintln!("translated: {translated}, skipped: {skipped}");
}

// --- pay_part_start (xpay) ---

#[test]
fn pay_part_start_parses_xpay_hops() {
    let entries = load_fixtures("node-A-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| e.topic == "pay_part_start")
        .expect("no pay_part_start in node-A fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("pay_part_start should translate");

    match &event {
        TraceEvent::PaymentPathAttempt {
            payment_hash,
            groupid,
            partid,
            route,
        } => {
            assert!(!payment_hash.is_empty());
            assert!(groupid.is_some());
            assert!(partid.is_some());
            assert_eq!(route.len(), 2, "A->B->C route should have 2 hops");

            // First hop: A->B via 108x2x0
            let hop0 = &route[0];
            assert_eq!(hop0.channel.scid.unwrap().to_string(), "108x2x0");
            // next_node should be B's pubkey
            assert!(hop0.node_id.0.starts_with("038c933c"));

            // Second hop: B->C via 108x1x0
            let hop1 = &route[1];
            assert_eq!(hop1.channel.scid.unwrap().to_string(), "108x1x0");
            // next_node should be C's pubkey
            assert!(hop1.node_id.0.starts_with("02386f20"));

            // Fee arithmetic: channel_in_msat - channel_out_msat
            // Hop 0: in=50000501, out=50000501, fee=0 (first hop pays no routing fee to itself)
            assert_eq!(hop0.fee_msat, 0);
            // Hop 1: in=50000501, out=50000000, fee=501
            assert_eq!(hop1.fee_msat, 501);
            assert_eq!(hop1.amount_msat, 50000000);
        }
        other => panic!("expected PaymentPathAttempt, got {other:?}"),
    }
}

// --- pay_part_end ---

#[test]
fn pay_part_end_success() {
    let entries = load_fixtures("node-A-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| {
            e.topic == "pay_part_end"
                && e.payload["pay_part_end"]["status"]
                    .as_str()
                    .map_or(false, |s| s == "success")
        })
        .expect("no successful pay_part_end in node-A fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("pay_part_end should translate");

    match &event {
        TraceEvent::PaymentPathResult {
            success,
            duration_secs,
            failcode,
            ..
        } => {
            assert!(*success, "status=success should map to success=true");
            assert!(duration_secs.is_some());
            assert!(duration_secs.unwrap() > 0.0);
            assert!(failcode.is_none());
        }
        other => panic!("expected PaymentPathResult, got {other:?}"),
    }
}

#[test]
fn pay_part_end_failure_with_erring_hop() {
    let entries = load_fixtures("node-A-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| {
            e.topic == "pay_part_end"
                && e.payload["pay_part_end"]["status"]
                    .as_str()
                    .map_or(false, |s| s == "failure")
        })
        .expect("no failed pay_part_end in node-A fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("pay_part_end should translate");

    match &event {
        TraceEvent::PaymentPathResult {
            success,
            failcode,
            erring_node,
            erring_channel,
            error_message,
            ..
        } => {
            assert!(!success, "status=failure should map to success=false");
            assert_eq!(*failcode, Some(4103)); // 0x1007 = temporary_channel_failure
                                               // Erring node is B
            assert!(erring_node.as_ref().unwrap().0.starts_with("038c933c"));
            // Erring channel is B->C: 108x1x0
            assert_eq!(
                erring_channel.as_ref().unwrap().scid.unwrap().to_string(),
                "108x1x0"
            );
            assert_eq!(error_message.as_deref(), Some("temporary_channel_failure"));
        }
        other => panic!("expected PaymentPathResult, got {other:?}"),
    }
}

// --- invoice_payment: sha256(preimage) ---

#[test]
fn invoice_payment_computes_payment_hash_from_preimage() {
    let entries = load_fixtures("node-C-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| e.topic == "invoice_payment")
        .expect("no invoice_payment in node-C fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("invoice_payment should translate");

    match &event {
        TraceEvent::PaymentReceived {
            payment_hash,
            amount_msat,
        } => {
            // Preimage 10ef4d9b... -> sha256 -> 63fef282...
            assert_eq!(
                payment_hash,
                "63fef282d588b839ecb1d3c7af2084cefeda33e1c11c7384f71f5a91d74f40a4"
            );
            assert_eq!(*amount_msat, 50000000);
        }
        other => panic!("expected PaymentReceived, got {other:?}"),
    }
}

// --- forward_event ---

#[test]
fn forward_event_settled_has_payment_hash() {
    let entries = load_fixtures("node-B-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| {
            e.topic == "forward_event"
                && e.payload["forward_event"]["status"]
                    .as_str()
                    .map_or(false, |s| s == "settled")
        })
        .expect("no settled forward_event in node-B fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("forward_event should translate");

    match &event {
        TraceEvent::ForwardEvent {
            in_channel,
            out_channel,
            in_msat,
            out_msat,
            fee_msat,
            status,
            payment_hash,
            ..
        } => {
            assert_eq!(*status, ForwardStatus::Settled);
            assert!(payment_hash.is_some());
            assert_eq!(in_channel.scid.unwrap().to_string(), "108x2x0");
            assert_eq!(out_channel.scid.unwrap().to_string(), "108x1x0");
            assert_eq!(*in_msat, 50000501);
            assert_eq!(*out_msat, 50000000);
            assert_eq!(*fee_msat, 501);
        }
        other => panic!("expected ForwardEvent, got {other:?}"),
    }
}

#[test]
fn forward_event_local_failed_has_failcode() {
    let entries = load_fixtures("node-B-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| {
            e.topic == "forward_event"
                && e.payload["forward_event"]["status"]
                    .as_str()
                    .map_or(false, |s| s == "local_failed")
        })
        .expect("no local_failed forward_event in node-B fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("forward_event should translate");

    match &event {
        TraceEvent::ForwardEvent {
            status,
            failcode,
            failreason,
            payment_hash,
            ..
        } => {
            assert_eq!(*status, ForwardStatus::LocalFailed);
            assert_eq!(*failcode, Some(4103));
            assert_eq!(
                failreason.as_deref(),
                Some("WIRE_TEMPORARY_CHANNEL_FAILURE")
            );
            assert!(payment_hash.is_some());
        }
        other => panic!("expected ForwardEvent, got {other:?}"),
    }
}

// --- sendpay_success / sendpay_failure ---

#[test]
fn sendpay_success_translates_to_payment_sent() {
    let entries = load_fixtures("node-A-raw.jsonl");
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
    let entries = load_fixtures("node-A-raw.jsonl");
    let entry = entries
        .iter()
        .find(|e| e.topic == "sendpay_failure")
        .expect("no sendpay_failure in fixtures");

    let event = translate(&entry.topic, &entry.payload).expect("sendpay_failure should translate");

    match &event {
        TraceEvent::PaymentFailed {
            payment_hash,
            reason,
            ..
        } => {
            assert!(!payment_hash.is_empty());
            assert!(reason.is_some());
        }
        other => panic!("expected PaymentFailed, got {other:?}"),
    }
}

// --- channel_state_changed ---

#[test]
fn channel_state_changed_skips_non_close() {
    let entries = load_all_fixtures();
    let entry = entries
        .iter()
        .find(|e| e.topic == "channel_state_changed")
        .expect("no channel_state_changed in fixtures");

    let event = translate(&entry.topic, &entry.payload);
    assert!(
        event.is_none(),
        "non-close channel_state_changed should be skipped"
    );
}

// --- Envelope round-trip ---

#[test]
fn envelope_serde_round_trip() {
    let entries = load_all_fixtures();
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

// --- Cross-node payment hash consistency ---

#[test]
fn payment_hash_matches_across_nodes() {
    let a_entries = load_fixtures("node-A-raw.jsonl");
    let b_entries = load_fixtures("node-B-raw.jsonl");
    let c_entries = load_fixtures("node-C-raw.jsonl");

    // The successful payment: A pays C via B.
    // Get payment_hash from A's first pay_part_start.
    let a_pps = a_entries
        .iter()
        .find(|e| e.topic == "pay_part_start")
        .unwrap();
    let a_event = translate(&a_pps.topic, &a_pps.payload).unwrap();
    let sender_hash = match &a_event {
        TraceEvent::PaymentPathAttempt { payment_hash, .. } => payment_hash.clone(),
        _ => panic!("expected PaymentPathAttempt"),
    };

    // B's settled forward_event should have the same hash.
    let b_settled = b_entries
        .iter()
        .find(|e| {
            e.topic == "forward_event"
                && e.payload["forward_event"]["status"]
                    .as_str()
                    .map_or(false, |s| s == "settled")
        })
        .unwrap();
    let b_event = translate(&b_settled.topic, &b_settled.payload).unwrap();
    let forward_hash = match &b_event {
        TraceEvent::ForwardEvent { payment_hash, .. } => payment_hash.clone().unwrap(),
        _ => panic!("expected ForwardEvent"),
    };

    // C's first invoice_payment (sha256 of preimage) should give the same hash.
    let c_invoice = c_entries
        .iter()
        .find(|e| e.topic == "invoice_payment")
        .unwrap();
    let c_event = translate(&c_invoice.topic, &c_invoice.payload).unwrap();
    let receiver_hash = match &c_event {
        TraceEvent::PaymentReceived { payment_hash, .. } => payment_hash.clone(),
        _ => panic!("expected PaymentReceived"),
    };

    assert_eq!(sender_hash, forward_hash);
    assert_eq!(sender_hash, receiver_hash);
}

// --- Direction invariant ---

/// B→C hop direction from pay_part_start must equal failed_direction from
/// pay_part_end for the same scid. If these diverge, the correlator's
/// `matches_erring_hop` will silently fail to match route hops to failures.
#[test]
fn direction_invariant_route_hop_matches_failure() {
    let entries = load_fixtures("node-A-raw.jsonl");

    // Extract B→C hop direction from pay_part_start route.
    let pps = entries
        .iter()
        .find(|e| e.topic == "pay_part_start")
        .expect("no pay_part_start in node-A fixtures");
    let pps_event = translate(&pps.topic, &pps.payload).unwrap();
    let (route_scid, route_direction) = match &pps_event {
        TraceEvent::PaymentPathAttempt { route, .. } => {
            // B→C hop is the second hop (index 1).
            let hop = &route[1];
            (hop.channel.scid.unwrap().to_string(), hop.direction)
        }
        _ => panic!("expected PaymentPathAttempt"),
    };

    // Extract failed_direction from the failed pay_part_end.
    let ppe = entries
        .iter()
        .find(|e| {
            e.topic == "pay_part_end"
                && e.payload["pay_part_end"]["status"]
                    .as_str()
                    .map_or(false, |s| s == "failure")
        })
        .expect("no failed pay_part_end in node-A fixtures");
    let ppe_event = translate(&ppe.topic, &ppe.payload).unwrap();
    let (erring_scid, failed_direction) = match &ppe_event {
        TraceEvent::PaymentPathResult {
            erring_channel,
            failed_direction,
            ..
        } => (
            erring_channel.as_ref().unwrap().scid.unwrap().to_string(),
            *failed_direction,
        ),
        _ => panic!("expected PaymentPathResult"),
    };

    // Both refer to the same channel.
    assert_eq!(route_scid, "108x1x0");
    assert_eq!(erring_scid, "108x1x0");

    // The invariant: route hop direction == failed_direction.
    assert_eq!(
        route_direction, failed_direction,
        "route hop direction ({route_direction:?}) must equal failed_direction ({failed_direction:?}) for scid {route_scid}"
    );

    // Both should be Some(1) for B→C on 108x1x0.
    assert_eq!(route_direction, Some(1));
}
