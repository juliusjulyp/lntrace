//! Translate raw CLN notification JSON into lntrace TraceEvent values.
//!
//! These functions are pure: JSON in, Option<TraceEvent> out, no I/O.
//! Used by both the live plugin and the test suite.

use lntrace_core::*;
use serde_json::Value;

/// Translate a raw CLN notification into a TraceEvent.
///
/// `topic` is the CLN notification name (e.g. "sendpay_success").
/// `payload` is the full notification JSON (the outer object that CLN sends).
///
/// Returns `None` for notifications we don't care about (e.g. channel_state_changed
/// for states other than ONCHAIN/CLOSED).
pub fn translate(topic: &str, payload: &Value) -> Option<TraceEvent> {
    match topic {
        "forward_event" => translate_forward_event(payload),
        "pay_part_start" => translate_pay_part_start(payload),
        "pay_part_end" => translate_pay_part_end(payload),
        "sendpay_success" => translate_sendpay_success(payload),
        "sendpay_failure" => translate_sendpay_failure(payload),
        "invoice_payment" => translate_invoice_payment(payload),
        "channel_opened" => translate_channel_opened(payload),
        "channel_state_changed" => translate_channel_state_changed(payload),
        _ => None,
    }
}

fn translate_forward_event(v: &Value) -> Option<TraceEvent> {
    let fe = &v["forward_event"];
    let status = match fe["status"].as_str().unwrap_or("") {
        "offered" => ForwardStatus::Offered,
        "settled" => ForwardStatus::Settled,
        "failed" => ForwardStatus::Failed,
        "local_failed" => ForwardStatus::LocalFailed,
        _ => return None,
    };

    Some(TraceEvent::ForwardEvent {
        in_channel: parse_channel_id(fe["in_channel"].as_str()),
        out_channel: parse_channel_id(fe["out_channel"].as_str()),
        in_msat: fe["in_msat"].as_u64().unwrap_or(0),
        out_msat: fe["out_msat"].as_u64().unwrap_or(0),
        fee_msat: fe["fee_msat"].as_u64().unwrap_or(0),
        status,
        payment_hash: fe["payment_hash"].as_str().map(String::from),
        failcode: fe["failcode"].as_u64().map(|c| c as u16),
        failreason: fe["failreason"].as_str().map(String::from),
    })
}

fn translate_pay_part_start(v: &Value) -> Option<TraceEvent> {
    let pps = &v["pay_part_start"];
    Some(TraceEvent::PaymentPathAttempt {
        payment_hash: pps["payment_hash"].as_str().unwrap_or("").to_string(),
        groupid: pps["groupid"].as_u64(),
        partid: pps["partid"].as_u64(),
        route: parse_hops(&pps["hops"]),
    })
}

fn translate_pay_part_end(v: &Value) -> Option<TraceEvent> {
    let ppe = &v["pay_part_end"];
    Some(TraceEvent::PaymentPathResult {
        payment_hash: ppe["payment_hash"].as_str().unwrap_or("").to_string(),
        groupid: ppe["groupid"].as_u64(),
        partid: ppe["partid"].as_u64(),
        success: ppe["status"].as_str() == Some("complete"),
        failcode: ppe["error_code"].as_u64().map(|c| c as u16),
        erring_node: ppe["failed_node_id"]
            .as_str()
            .map(|s| NodeId(s.to_string())),
        erring_channel: ppe["failed_short_channel_id"]
            .as_str()
            .map(|s| parse_channel_id(Some(s))),
        error_message: ppe["error_message"].as_str().map(String::from),
        duration_secs: ppe["duration"].as_f64(),
    })
}

fn translate_sendpay_success(v: &Value) -> Option<TraceEvent> {
    let sp = &v["sendpay_success"];
    Some(TraceEvent::PaymentSent {
        payment_hash: sp["payment_hash"].as_str().unwrap_or("").to_string(),
        payment_preimage: sp["payment_preimage"].as_str().map(String::from),
        amount_msat: sp["amount_sent_msat"].as_u64().unwrap_or(0),
        fee_msat: None,
    })
}

fn translate_sendpay_failure(v: &Value) -> Option<TraceEvent> {
    let sf = &v["sendpay_failure"];
    let data = &sf["data"];
    Some(TraceEvent::PaymentFailed {
        payment_hash: data["payment_hash"].as_str().unwrap_or("").to_string(),
        failcode: data["failcode"].as_u64().map(|c| c as u16),
        reason: sf["message"].as_str().map(String::from),
    })
}

fn translate_invoice_payment(v: &Value) -> Option<TraceEvent> {
    let ip = &v["invoice_payment"];
    Some(TraceEvent::PaymentReceived {
        payment_hash: ip["payment_hash"].as_str().unwrap_or("").to_string(),
        amount_msat: ip["msat"].as_u64().unwrap_or(0),
    })
}

fn translate_channel_opened(v: &Value) -> Option<TraceEvent> {
    let co = &v["channel_opened"];
    Some(TraceEvent::ChannelReady {
        channel: parse_channel_id(co["funding_txid"].as_str()),
        peer: NodeId(co["id"].as_str().unwrap_or("").to_string()),
        capacity_sat: co["funding_msat"].as_u64().map(|m| m / 1000).unwrap_or(0),
    })
}

fn translate_channel_state_changed(v: &Value) -> Option<TraceEvent> {
    let cs = &v["channel_state_changed"];
    match cs["new_state"].as_str().unwrap_or("") {
        "ONCHAIN" | "CLOSED" => Some(TraceEvent::ChannelClosed {
            channel: parse_channel_id(cs["short_channel_id"].as_str()),
            peer: NodeId(cs["peer_id"].as_str().unwrap_or("").to_string()),
            reason: match cs["cause"].as_str().unwrap_or("") {
                "local" | "remote" => CloseReason::Cooperative,
                "onchain" => CloseReason::Force,
                other => CloseReason::Unknown(other.to_string()),
            },
        }),
        _ => None,
    }
}

// --- Helpers ---

pub(crate) fn parse_channel_id(scid_str: Option<&str>) -> ChannelId {
    ChannelId {
        scid: scid_str.and_then(ShortChannelId::from_str_bolt),
        funding: None,
    }
}

pub(crate) fn parse_hops(value: &Value) -> Vec<Hop> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .map(|h| Hop {
            node_id: NodeId(h["id"].as_str().unwrap_or("").to_string()),
            channel: parse_channel_id(h["channel"].as_str()),
            amount_msat: h["amount_msat"].as_u64().unwrap_or(0),
            fee_msat: h["fee_msat"].as_u64().unwrap_or(0),
            cltv_expiry: h["delay"].as_u64().unwrap_or(0) as u32,
        })
        .collect()
}
