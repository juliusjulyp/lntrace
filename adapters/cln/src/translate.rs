//! Translate raw CLN notification JSON into lntrace TraceEvent values.
//!
//! Uses typed deserialization via `cln_rpc::notifications` structs.
//! Pure functions: JSON in, `Option<TraceEvent>` out, no I/O.

use cln_rpc::model::responses::ListpeerchannelsResponse;
use cln_rpc::notifications::{
    ChannelOpenedNotification, ChannelStateChangedCause, ChannelStateChangedNotification,
    ForwardEventNotification, ForwardEventStatus, InvoicePaymentNotification,
    PayPartEndNotification, PayPartEndStatus, PayPartStartNotification, SendPayFailureNotification,
    SendPaySuccessNotification,
};
use cln_rpc::primitives::ChannelState;
use lntrace::*;
use serde_json::Value;
use sha2::{Digest, Sha256 as Sha256Digest};

/// Translate a raw CLN notification into a TraceEvent.
///
/// `topic` is the CLN notification name (e.g. "forward_event").
/// `payload` is the full notification JSON (the outer object that CLN sends,
/// e.g. `{"forward_event": {...}}`).
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
    let fe: ForwardEventNotification = serde_json::from_value(v["forward_event"].clone()).ok()?;

    let status = match fe.status {
        ForwardEventStatus::OFFERED => ForwardStatus::Offered,
        ForwardEventStatus::SETTLED => ForwardStatus::Settled,
        ForwardEventStatus::FAILED => ForwardStatus::Failed,
        ForwardEventStatus::LOCAL_FAILED => ForwardStatus::LocalFailed,
    };

    Some(TraceEvent::ForwardEvent {
        in_channel: cln_scid_to_channel_id(fe.in_channel),
        out_channel: fe
            .out_channel
            .map(cln_scid_to_channel_id)
            .unwrap_or(ChannelId {
                scid: None,
                funding: None,
            }),
        in_msat: fe.in_msat.msat(),
        out_msat: fe.out_msat.map(|a| a.msat()).unwrap_or(0),
        fee_msat: fe.fee_msat.map(|a| a.msat()).unwrap_or(0),
        status,
        payment_hash: Some(fe.payment_hash.to_string()),
        failcode: fe.failcode.map(|c| c as u16),
        failreason: fe.failreason,
    })
}

fn translate_pay_part_start(v: &Value) -> Option<TraceEvent> {
    let pps: PayPartStartNotification = serde_json::from_value(v["pay_part_start"].clone()).ok()?;

    Some(TraceEvent::PaymentPathAttempt {
        payment_hash: pps.payment_hash.to_string(),
        groupid: Some(pps.groupid),
        partid: Some(pps.partid),
        route: pps
            .hops
            .iter()
            .map(|h| Hop {
                node_id: NodeId(h.next_node.to_string()),
                channel: cln_scid_to_channel_id(h.short_channel_id),
                amount_msat: h.channel_out_msat.msat(),
                fee_msat: h
                    .channel_in_msat
                    .msat()
                    .saturating_sub(h.channel_out_msat.msat()),
                cltv_expiry: 0, // not provided in xpay notifications
                direction: Some(h.direction),
            })
            .collect(),
    })
}

fn translate_pay_part_end(v: &Value) -> Option<TraceEvent> {
    let ppe: PayPartEndNotification = serde_json::from_value(v["pay_part_end"].clone()).ok()?;

    Some(TraceEvent::PaymentPathResult {
        payment_hash: ppe.payment_hash.to_string(),
        groupid: Some(ppe.groupid),
        partid: Some(ppe.partid),
        success: ppe.status == PayPartEndStatus::SUCCESS,
        failcode: ppe.error_code.map(|c| c as u16),
        erring_node: ppe.failed_node_id.map(|pk| NodeId(pk.to_string())),
        erring_channel: ppe.failed_short_channel_id.map(cln_scid_to_channel_id),
        error_message: ppe.error_message,
        duration_secs: Some(ppe.duration),
        failed_direction: ppe.failed_direction,
    })
}

fn translate_sendpay_success(v: &Value) -> Option<TraceEvent> {
    let sp: SendPaySuccessNotification =
        serde_json::from_value(v["sendpay_success"].clone()).ok()?;

    Some(TraceEvent::PaymentSent {
        payment_hash: sp.payment_hash.to_string(),
        payment_preimage: sp
            .payment_preimage
            .map(|s| bytes_to_hex(&<[u8; 32]>::from(s))),
        amount_msat: sp.amount_sent_msat.msat(),
        fee_msat: None,
    })
}

fn translate_sendpay_failure(v: &Value) -> Option<TraceEvent> {
    let sf: SendPayFailureNotification =
        serde_json::from_value(v["sendpay_failure"].clone()).ok()?;

    Some(TraceEvent::PaymentFailed {
        payment_hash: sf
            .data
            .payment_hash
            .map(|h| h.to_string())
            .unwrap_or_default(),
        failcode: sf.data.failcode.map(|c| c as u16),
        reason: Some(sf.message),
    })
}

fn translate_invoice_payment(v: &Value) -> Option<TraceEvent> {
    let ip: InvoicePaymentNotification =
        serde_json::from_value(v["invoice_payment"].clone()).ok()?;

    // CLN v26 invoice_payment omits payment_hash; compute as sha256(preimage).
    let preimage_bytes: [u8; 32] = ip.preimage.into();
    let hash = Sha256Digest::digest(preimage_bytes);
    let payment_hash = bytes_to_hex(&hash);

    Some(TraceEvent::PaymentReceived {
        payment_hash,
        amount_msat: ip.msat.msat(),
    })
}

fn translate_channel_opened(v: &Value) -> Option<TraceEvent> {
    let co: ChannelOpenedNotification = serde_json::from_value(v["channel_opened"].clone()).ok()?;

    Some(TraceEvent::ChannelReady {
        channel: ChannelId {
            scid: None,
            funding: None,
        },
        peer: NodeId(co.id.to_string()),
        capacity_sat: co.funding_msat.msat() / 1000,
    })
}

fn translate_channel_state_changed(v: &Value) -> Option<TraceEvent> {
    let cs: ChannelStateChangedNotification =
        serde_json::from_value(v["channel_state_changed"].clone()).ok()?;

    match cs.new_state {
        ChannelState::ONCHAIN | ChannelState::CLOSED => Some(TraceEvent::ChannelClosed {
            channel: cs
                .short_channel_id
                .map(cln_scid_to_channel_id)
                .unwrap_or(ChannelId {
                    scid: None,
                    funding: None,
                }),
            peer: NodeId(cs.peer_id.to_string()),
            reason: match cs.cause {
                ChannelStateChangedCause::LOCAL | ChannelStateChangedCause::REMOTE => {
                    CloseReason::Cooperative
                }
                ChannelStateChangedCause::ONCHAIN => CloseReason::Force,
                other => CloseReason::Unknown(format!("{:?}", other)),
            },
        }),
        _ => None,
    }
}

// --- listpeerchannels translation ---

/// Translate a typed `ListpeerchannelsResponse` into a vec of `ChannelSnapshot`.
pub fn translate_listpeerchannels(resp: &ListpeerchannelsResponse) -> Vec<ChannelSnapshot> {
    resp.channels
        .iter()
        .filter_map(|ch| {
            // Only snapshot channels that have an scid (usable channels).
            let scid = ch.short_channel_id?;
            let channel_id = cln_scid_to_channel_id(scid);

            Some(ChannelSnapshot {
                channel: channel_id,
                peer: NodeId(ch.peer_id.to_string()),
                capacity_sat: ch.total_msat.map(|a| a.msat() / 1000).unwrap_or(0),
                local_msat: ch.to_us_msat.map(|a| a.msat()).unwrap_or(0),
                remote_msat: ch
                    .total_msat
                    .and_then(|total| {
                        ch.to_us_msat
                            .map(|local| total.msat().saturating_sub(local.msat()))
                    })
                    .unwrap_or(0),
                active: ch.state == ChannelState::CHANNELD_NORMAL && ch.peer_connected,
                direction: ch.direction,
                state: Some(format!("{:?}", ch.state)),
                spendable_msat: ch.spendable_msat.map(|a| a.msat()),
                receivable_msat: ch.receivable_msat.map(|a| a.msat()),
                minimum_htlc_out_msat: ch.minimum_htlc_out_msat.map(|a| a.msat()),
                max_accepted_htlcs: ch.max_accepted_htlcs,
                inflight_htlc_count: ch.htlcs.as_ref().map(|htlcs| htlcs.len() as u32),
            })
        })
        .collect()
}

/// Translate a raw `listpeerchannels` JSON payload (as stored by the recorder)
/// into a vec of `ChannelSnapshot`. Returns empty vec on deserialization failure.
///
/// This wraps `translate_listpeerchannels` so callers don't need `cln_rpc` types.
pub fn translate_listpeerchannels_raw(payload: &Value) -> Vec<ChannelSnapshot> {
    let resp: ListpeerchannelsResponse = match serde_json::from_value(payload.clone()) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    translate_listpeerchannels(&resp)
}

// --- Helpers ---

fn cln_scid_to_channel_id(scid: cln_rpc::primitives::ShortChannelId) -> ChannelId {
    let s = scid.to_string();
    ChannelId {
        scid: lntrace::ShortChannelId::from_str_bolt(&s),
        funding: None,
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
