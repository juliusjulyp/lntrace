use lntrace_core::{Confidence, Envelope, NodeId, TraceEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A correlated payment trace: one payment hash, all hops, all shards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub payment_hash: String,
    pub hops: Vec<TracedHop>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureInfo>,
}

/// A single hop in a correlated trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracedHop {
    pub index: usize,
    pub node_id: NodeId,
    pub amount_msat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_msat: Option<u64>,
    pub confidence: Confidence,
    /// If this hop was the one that failed, the enriched reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<HopFailure>,
}

/// Failure info at the payment level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureInfo {
    pub failcode: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub erring_node: Option<NodeId>,
    pub explanation: String,
}

/// Failure info enriched with data from the failing node's forward event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HopFailure {
    /// BOLT 4 code the sender saw.
    pub sender_failcode: u16,
    /// The actual reason from the failing node's forward_event (if instrumented).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_failreason: Option<String>,
}

/// Pure function: events in, traces out. No I/O.
///
/// Groups envelopes by payment hash, stitches sender path data with
/// intermediate forward events, and marks inferred hops.
///
/// Stage 2 implementation: uses only sender data (PaymentPathAttempt +
/// PaymentPathResult). Stage 3 adds cross-node correlation.
pub fn correlate(events: &[Envelope]) -> Vec<Trace> {
    // Group by payment_hash.
    let mut by_hash: HashMap<String, Vec<&Envelope>> = HashMap::new();

    for env in events {
        if let Some(hash) = extract_payment_hash(&env.event) {
            by_hash.entry(hash).or_default().push(env);
        }
    }

    let mut traces = Vec::new();
    for (payment_hash, envs) in by_hash {
        if let Some(trace) = build_trace(&payment_hash, &envs) {
            traces.push(trace);
        }
    }

    // Sort by first event timestamp.
    traces.sort_by_key(|t| {
        events
            .iter()
            .filter(|e| extract_payment_hash(&e.event).as_deref() == Some(&t.payment_hash))
            .map(|e| e.node_ts_ms)
            .min()
            .unwrap_or(0)
    });

    traces
}

fn extract_payment_hash(event: &TraceEvent) -> Option<String> {
    match event {
        TraceEvent::ForwardEvent { payment_hash, .. } => payment_hash.clone(),
        TraceEvent::PaymentSent { payment_hash, .. }
        | TraceEvent::PaymentFailed { payment_hash, .. }
        | TraceEvent::PaymentReceived { payment_hash, .. }
        | TraceEvent::PaymentPathAttempt { payment_hash, .. }
        | TraceEvent::PaymentPathResult { payment_hash, .. } => Some(payment_hash.clone()),
        _ => None,
    }
}

/// Build a trace from grouped envelopes. Stage 2: sender-only.
fn build_trace(payment_hash: &str, _envs: &[&Envelope]) -> Option<Trace> {
    // TODO(stage-2): Build hops from PaymentPathAttempt route.
    // TODO(stage-3): Merge with ForwardEvent data from intermediates.
    // TODO(stage-4): Enrich local_failed with actual failreason.
    Some(Trace {
        payment_hash: payment_hash.to_string(),
        hops: Vec::new(),
        success: false,
        failure: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_events_produce_no_traces() {
        let traces = correlate(&[]);
        assert!(traces.is_empty());
    }
}
