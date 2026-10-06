use crate::{
    cause, ChannelSnapshot, Confidence, Envelope, ForwardStatus, NodeId, ShortChannelId, TraceEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A correlated payment trace: one payment hash, all attempts/shards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub payment_hash: String,
    pub attempts: Vec<Attempt>,
    /// True if any attempt succeeded or a PaymentSent event was seen.
    pub success: bool,
    /// Failure info from the last failed attempt (if the payment failed overall).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureInfo>,
}

/// The outcome of a single payment attempt (shard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    Failed,
    /// Sender abandoned this shard (start with no end, payment has a final outcome).
    Cancelled,
    /// Started but no result yet (only seen during live tailing).
    InFlight,
}

/// A single attempt (shard) within a payment. Keyed by (groupid, partid).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groupid: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partid: Option<u64>,
    pub hops: Vec<TracedHop>,
    pub outcome: AttemptOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

impl Attempt {
    pub fn succeeded(&self) -> bool {
        self.outcome == AttemptOutcome::Succeeded
    }
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
    /// Inferred cause from channel state snapshot (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inferred_cause: Option<String>,
}

/// Pure function: events in, traces out. No I/O.
///
/// Groups envelopes by payment hash, pairs each PaymentPathAttempt with its
/// PaymentPathResult by (groupid, partid), stitches sender path data with
/// intermediate forward events, and builds per-attempt hop lists.
pub fn correlate(events: &[Envelope]) -> Vec<Trace> {
    // Build snapshot index: (node_id, scid) -> latest ChannelSnapshot.
    let snapshots = build_snapshot_index(events);

    // Group by payment_hash.
    let mut by_hash: HashMap<String, Vec<&Envelope>> = HashMap::new();

    for env in events {
        if let Some(hash) = extract_payment_hash(&env.event) {
            by_hash.entry(hash).or_default().push(env);
        }
    }

    let mut traces = Vec::new();
    for (payment_hash, envs) in &by_hash {
        if let Some(trace) = build_trace(payment_hash, envs, &snapshots) {
            traces.push(trace);
        }
    }

    // Sort by first event timestamp.
    traces.sort_by_key(|t| {
        by_hash
            .get(&t.payment_hash)
            .and_then(|envs| envs.iter().map(|e| e.node_ts_ms).min())
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

/// Snapshot index key: (node_id, scid).
type SnapshotKey = (NodeId, ShortChannelId);

/// Snapshot index: stores all snapshots per (node, scid), sorted by seq.
/// This allows looking up the snapshot closest to a specific event.
type SnapshotIndex = HashMap<SnapshotKey, Vec<(u64, ChannelSnapshot)>>;

/// Build an index of all channel snapshots per (node, scid) pair, sorted by seq.
fn build_snapshot_index(events: &[Envelope]) -> SnapshotIndex {
    let mut index: SnapshotIndex = HashMap::new();

    for env in events {
        if let TraceEvent::Snapshot { channels } = &env.event {
            for snap in channels {
                if let Some(scid) = snap.channel.scid {
                    index
                        .entry((env.node_id.clone(), scid))
                        .or_default()
                        .push((env.seq, snap.clone()));
                }
            }
        }
    }

    // Sort each vec by seq for binary-search-friendly lookup.
    for entries in index.values_mut() {
        entries.sort_by_key(|(seq, _)| *seq);
    }

    index
}

/// Find the first snapshot from `node_id` for `scid` with seq >= `min_seq`.
///
/// This ensures we use the snapshot taken right after the failure event,
/// not one from later when channel state may have changed.
fn find_snapshot_after<'a>(
    snapshots: &'a SnapshotIndex,
    node_id: &NodeId,
    scid: ShortChannelId,
    min_seq: u64,
) -> Option<&'a ChannelSnapshot> {
    snapshots.get(&(node_id.clone(), scid)).and_then(|entries| {
        entries
            .iter()
            .find(|(seq, _)| *seq >= min_seq)
            .map(|(_, snap)| snap)
    })
}

/// Shard key for pairing attempts with results.
type ShardKey = (Option<u64>, Option<u64>);

/// Build a trace from grouped envelopes sharing the same payment_hash.
///
/// Pairs each PaymentPathAttempt with its PaymentPathResult by (groupid, partid).
/// Merges ForwardEvent data from intermediate nodes by matching on out_channel
/// scid + direction, and verifies the forward comes from the erring node.
fn build_trace(payment_hash: &str, envs: &[&Envelope], snapshots: &SnapshotIndex) -> Option<Trace> {
    // Collect attempts and results keyed by (groupid, partid).
    let mut attempts_by_shard: HashMap<ShardKey, &Vec<crate::Hop>> = HashMap::new();
    let mut results_by_shard: HashMap<ShardKey, ResultInfo> = HashMap::new();
    let mut saw_payment_sent = false;
    let mut saw_payment_failed = false;

    for env in envs {
        match &env.event {
            TraceEvent::PaymentPathAttempt {
                groupid,
                partid,
                route,
                ..
            } => {
                attempts_by_shard.insert((*groupid, *partid), route);
            }
            TraceEvent::PaymentPathResult {
                groupid,
                partid,
                success,
                failcode,
                erring_node,
                erring_channel,
                error_message,
                duration_secs,
                failed_direction,
                ..
            } => {
                results_by_shard.insert(
                    (*groupid, *partid),
                    ResultInfo {
                        success: *success,
                        failcode: *failcode,
                        erring_node: erring_node.clone(),
                        erring_channel: erring_channel.clone(),
                        error_message: error_message.clone(),
                        duration_secs: *duration_secs,
                        failed_direction: *failed_direction,
                    },
                );
            }
            TraceEvent::PaymentSent { .. } => {
                saw_payment_sent = true;
            }
            TraceEvent::PaymentFailed { .. } => {
                saw_payment_failed = true;
            }
            _ => {}
        }
    }

    // Need at least one attempt to build a trace.
    if attempts_by_shard.is_empty() {
        return None;
    }

    // Build all shard keys in deterministic order.
    let mut shard_keys: Vec<ShardKey> = attempts_by_shard.keys().cloned().collect();
    shard_keys.sort();

    let mut attempts = Vec::new();
    let mut any_success = saw_payment_sent;
    let mut last_failure: Option<FailureInfo> = None;

    for key in &shard_keys {
        let route = attempts_by_shard[key];
        let result = results_by_shard.get(key);

        if route.is_empty() {
            continue;
        }

        let attempt_outcome = match result {
            Some(r) if r.success => AttemptOutcome::Succeeded,
            Some(_) => AttemptOutcome::Failed,
            None => AttemptOutcome::InFlight, // tentative; reclassified below
        };
        if attempt_outcome == AttemptOutcome::Succeeded {
            any_success = true;
        }

        // Build hops for this attempt.
        let hops: Vec<TracedHop> = route
            .iter()
            .enumerate()
            .map(|(i, hop)| {
                let mut hop_failure = None;

                // Check if this hop is the failing one.
                if let Some(res) = result {
                    if !res.success {
                        if let (Some(ref erring_ch), Some(failcode)) =
                            (&res.erring_channel, res.failcode)
                        {
                            if matches_erring_hop(hop, erring_ch, res.failed_direction) {
                                // Look for a matching forward_event from the erring node.
                                let fwd_match = find_forward_match(
                                    envs,
                                    &hop.channel,
                                    payment_hash,
                                    res.erring_node.as_ref(),
                                );

                                let local_failreason =
                                    fwd_match.as_ref().and_then(|m| m.failreason.clone());

                                // Infer cause from the erring node's snapshot taken
                                // at or after its local_failed forward event.
                                let inferred_cause = res
                                    .erring_node
                                    .as_ref()
                                    .and_then(|node| {
                                        let scid = hop.channel.scid?;
                                        let min_seq =
                                            fwd_match.as_ref().map(|m| m.node_seq).unwrap_or(0);
                                        find_snapshot_after(snapshots, node, scid, min_seq)
                                    })
                                    .and_then(|snap| {
                                        cause::infer_cause(failcode, hop.amount_msat, snap)
                                    })
                                    .map(|c| c.description);

                                hop_failure = Some(HopFailure {
                                    sender_failcode: failcode,
                                    local_failreason,
                                    inferred_cause,
                                });
                            }
                        }
                    }
                }

                // Confidence: sender route data is Exact.
                // Forward confirmation from intermediate node is also Exact.
                let confidence = Confidence::Exact;

                TracedHop {
                    index: i,
                    node_id: hop.node_id.clone(),
                    amount_msat: hop.amount_msat,
                    fee_msat: Some(hop.fee_msat),
                    confidence,
                    failure: hop_failure,
                }
            })
            .collect();

        // Build attempt-level failure info.
        let attempt_failure = result.and_then(|r| {
            if r.success {
                return None;
            }
            r.failcode.map(|code| {
                let exp = crate::explain::explain(code);
                FailureInfo {
                    failcode: code,
                    erring_node: r.erring_node.clone(),
                    explanation: exp.explanation.to_string(),
                }
            })
        });

        if attempt_outcome == AttemptOutcome::Failed {
            if let Some(ref f) = attempt_failure {
                last_failure = Some(f.clone());
            }
        }

        attempts.push(Attempt {
            groupid: key.0,
            partid: key.1,
            hops,
            outcome: attempt_outcome,
            failure: attempt_failure,
            duration_secs: result.and_then(|r| r.duration_secs),
        });
    }

    // Reclassify InFlight → Cancelled if the payment reached a final outcome.
    let payment_has_final_outcome =
        any_success || saw_payment_failed || !results_by_shard.is_empty();
    if payment_has_final_outcome {
        for attempt in &mut attempts {
            if attempt.outcome == AttemptOutcome::InFlight {
                attempt.outcome = AttemptOutcome::Cancelled;
            }
        }
    }

    // Overall failure: from the last failed attempt (if the payment didn't succeed).
    let overall_failure = if any_success { None } else { last_failure };

    Some(Trace {
        payment_hash: payment_hash.to_string(),
        attempts,
        success: any_success,
        failure: overall_failure,
    })
}

/// Check if a route hop matches the erring channel from the result.
///
/// Matches on scid, and if `failed_direction` is available, also on direction.
fn matches_erring_hop(
    hop: &crate::Hop,
    erring_channel: &crate::ChannelId,
    failed_direction: Option<u32>,
) -> bool {
    // Must have matching scid.
    if hop.channel.scid.is_none() || hop.channel.scid != erring_channel.scid {
        return false;
    }

    // If both direction fields are available, they must match.
    if let (Some(hop_dir), Some(fail_dir)) = (hop.direction, failed_direction) {
        if hop_dir != fail_dir {
            return false;
        }
    }

    true
}

struct ResultInfo {
    success: bool,
    failcode: Option<u16>,
    erring_node: Option<NodeId>,
    erring_channel: Option<crate::ChannelId>,
    #[allow(dead_code)]
    error_message: Option<String>,
    duration_secs: Option<f64>,
    failed_direction: Option<u32>,
}

/// Result of matching a forward_event from the erring node.
struct ForwardMatch {
    failreason: Option<String>,
    /// Seq number of the forward envelope (used to find the right snapshot).
    node_seq: u64,
}

/// Search for a forward_event with local_failed/failed status on the given
/// channel for the given payment_hash. Verifies the forward comes from the
/// expected erring node. Returns the failreason and the forward's seq.
fn find_forward_match(
    envs: &[&Envelope],
    channel: &crate::ChannelId,
    payment_hash: &str,
    erring_node: Option<&NodeId>,
) -> Option<ForwardMatch> {
    for env in envs {
        if let TraceEvent::ForwardEvent {
            out_channel,
            payment_hash: Some(ref fwd_hash),
            status,
            failreason,
            ..
        } = &env.event
        {
            if fwd_hash == payment_hash
                && (*status == ForwardStatus::LocalFailed || *status == ForwardStatus::Failed)
                && channel.scid.is_some()
                && out_channel.scid == channel.scid
            {
                // Verify the forward came from the expected erring node.
                if let Some(expected_node) = erring_node {
                    if env.node_id != *expected_node {
                        continue;
                    }
                }
                return Some(ForwardMatch {
                    failreason: failreason.clone(),
                    node_seq: env.seq,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelId, Hop, ShortChannelId, SCHEMA_VERSION};

    #[test]
    fn empty_events_produce_no_traces() {
        let traces = correlate(&[]);
        assert!(traces.is_empty());
    }

    fn make_envelope(node: &str, seq: u64, ts: u64, event: TraceEvent) -> Envelope {
        Envelope {
            schema_version: SCHEMA_VERSION,
            node_id: NodeId(node.to_string()),
            seq,
            node_ts_ms: ts,
            collector_ts_ms: None,
            event,
        }
    }

    fn scid(s: &str) -> ChannelId {
        ChannelId {
            scid: ShortChannelId::from_str_bolt(s),
            funding: None,
        }
    }

    #[test]
    fn single_shard_success_trace() {
        let envs = vec![
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "abc123".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: None,
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: None,
                        },
                    ],
                },
            ),
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "abc123".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    success: true,
                    failcode: None,
                    erring_node: None,
                    erring_channel: None,
                    error_message: None,
                    duration_secs: Some(0.5),
                    failed_direction: None,
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let t = &traces[0];
        assert_eq!(t.payment_hash, "abc123");
        assert!(t.success);
        assert!(t.failure.is_none());
        assert_eq!(t.attempts.len(), 1);

        let a = &t.attempts[0];
        assert_eq!(a.outcome, AttemptOutcome::Succeeded);
        assert_eq!(a.hops.len(), 2);
        assert_eq!(a.hops[0].node_id.0, "node_b");
        assert_eq!(a.hops[0].amount_msat, 50000501);
        assert_eq!(a.hops[1].node_id.0, "node_c");
        assert_eq!(a.hops[1].fee_msat, Some(501));
        assert_eq!(a.duration_secs, Some(0.5));
    }

    #[test]
    fn single_shard_failure_with_erring_hop() {
        let envs = vec![
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "fail456".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: None,
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: None,
                        },
                    ],
                },
            ),
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "fail456".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x2x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.3),
                    failed_direction: None,
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let t = &traces[0];
        assert!(!t.success);
        let f = t.failure.as_ref().unwrap();
        assert_eq!(f.failcode, 4103);
        assert_eq!(f.erring_node.as_ref().unwrap().0, "node_b");

        let a = &t.attempts[0];
        assert_eq!(a.outcome, AttemptOutcome::Failed);
        // Hop 1 (100x2x0) should be flagged as failing.
        assert!(a.hops[0].failure.is_none());
        let hop_fail = a.hops[1].failure.as_ref().unwrap();
        assert_eq!(hop_fail.sender_failcode, 4103);
        assert!(hop_fail.local_failreason.is_none()); // no forward data
    }

    #[test]
    fn cross_node_forward_enriches_failure() {
        let envs = vec![
            // Sender's attempt
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "cross789".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: None,
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: None,
                        },
                    ],
                },
            ),
            // Sender's result
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "cross789".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x2x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.3),
                    failed_direction: None,
                },
            ),
            // Intermediate node B's forward_event (local_failed)
            make_envelope(
                "node_b",
                0,
                1000,
                TraceEvent::ForwardEvent {
                    in_channel: scid("100x1x0"),
                    out_channel: scid("100x2x0"),
                    in_msat: 50000501,
                    out_msat: 0,
                    fee_msat: 0,
                    status: ForwardStatus::LocalFailed,
                    payment_hash: Some("cross789".into()),
                    failcode: Some(4103),
                    failreason: Some("WIRE_TEMPORARY_CHANNEL_FAILURE".into()),
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let t = &traces[0];
        assert!(!t.success);

        // Hop 1 should have enriched failure with local_failreason from B.
        let a = &t.attempts[0];
        let hop_fail = a.hops[1].failure.as_ref().unwrap();
        assert_eq!(hop_fail.sender_failcode, 4103);
        assert_eq!(
            hop_fail.local_failreason.as_deref(),
            Some("WIRE_TEMPORARY_CHANNEL_FAILURE")
        );
    }

    #[test]
    fn forward_from_wrong_node_is_not_matched() {
        // Verify that find_forward_failreason checks the envelope's node_id
        // matches the erring_node from the result.
        let envs = vec![
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "nodecheck".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: None,
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: None,
                        },
                    ],
                },
            ),
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "nodecheck".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x2x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.3),
                    failed_direction: None,
                },
            ),
            // Forward from node_c (wrong node — erring is node_b)
            make_envelope(
                "node_c",
                0,
                1000,
                TraceEvent::ForwardEvent {
                    in_channel: scid("100x1x0"),
                    out_channel: scid("100x2x0"),
                    in_msat: 50000501,
                    out_msat: 0,
                    fee_msat: 0,
                    status: ForwardStatus::LocalFailed,
                    payment_hash: Some("nodecheck".into()),
                    failcode: Some(4103),
                    failreason: Some("WIRE_TEMPORARY_CHANNEL_FAILURE".into()),
                },
            ),
        ];

        let traces = correlate(&envs);
        let a = &traces[0].attempts[0];
        let hop_fail = a.hops[1].failure.as_ref().unwrap();
        // local_failreason should be None because the forward came from node_c, not node_b.
        assert!(hop_fail.local_failreason.is_none());
    }

    #[test]
    fn retry_produces_two_attempts() {
        // Same payment_hash, two different (groupid, partid) pairs.
        // First attempt fails, second succeeds on a different route.
        let envs = vec![
            // Attempt 1: fails at B->C
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "retry_hash".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: Some(1),
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: Some(0),
                        },
                    ],
                },
            ),
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "retry_hash".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x2x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.3),
                    failed_direction: Some(0),
                },
            ),
            // Attempt 2: succeeds via B->D
            make_envelope(
                "sender",
                2,
                1002,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "retry_hash".into(),
                    groupid: Some(2),
                    partid: Some(0),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: Some(1),
                        },
                        Hop {
                            node_id: NodeId("node_d".into()),
                            channel: scid("100x3x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: Some(0),
                        },
                    ],
                },
            ),
            make_envelope(
                "sender",
                3,
                1003,
                TraceEvent::PaymentPathResult {
                    payment_hash: "retry_hash".into(),
                    groupid: Some(2),
                    partid: Some(0),
                    success: true,
                    failcode: None,
                    erring_node: None,
                    erring_channel: None,
                    error_message: None,
                    duration_secs: Some(0.4),
                    failed_direction: None,
                },
            ),
            make_envelope(
                "sender",
                4,
                1004,
                TraceEvent::PaymentSent {
                    payment_hash: "retry_hash".into(),
                    payment_preimage: Some("deadbeef".into()),
                    amount_msat: 50000000,
                    fee_msat: Some(501),
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let t = &traces[0];
        assert!(t.success);
        assert!(t.failure.is_none()); // overall success -> no failure
        assert_eq!(t.attempts.len(), 2);

        // Attempt 1: failed
        let a0 = &t.attempts[0];
        assert_eq!(a0.groupid, Some(1));
        assert_eq!(a0.outcome, AttemptOutcome::Failed);
        assert!(a0.failure.is_some());
        assert_eq!(a0.hops.len(), 2);
        assert!(a0.hops[1].failure.is_some());
        assert_eq!(a0.hops[1].failure.as_ref().unwrap().sender_failcode, 4103);

        // Attempt 2: succeeded on different route
        let a1 = &t.attempts[1];
        assert_eq!(a1.groupid, Some(2));
        assert_eq!(a1.outcome, AttemptOutcome::Succeeded);
        assert!(a1.failure.is_none());
        assert_eq!(a1.hops.len(), 2);
        assert_eq!(a1.hops[1].node_id.0, "node_d"); // different route
        assert!(a1.hops[1].failure.is_none());
    }

    #[test]
    fn direction_mismatch_prevents_hop_match() {
        // erring_channel scid matches, but direction doesn't.
        let envs = vec![
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "dir_test".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    route: vec![Hop {
                        node_id: NodeId("node_b".into()),
                        channel: scid("100x1x0"),
                        amount_msat: 50000000,
                        fee_msat: 0,
                        cltv_expiry: 0,
                        direction: Some(1),
                    }],
                },
            ),
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "dir_test".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x1x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.2),
                    // Direction 0 — doesn't match hop's direction 1.
                    failed_direction: Some(0),
                },
            ),
        ];

        let traces = correlate(&envs);
        let a = &traces[0].attempts[0];
        // The hop should NOT be flagged because direction mismatches.
        assert!(a.hops[0].failure.is_none());
    }

    #[test]
    fn traces_sorted_by_timestamp() {
        let envs = vec![
            make_envelope(
                "sender",
                0,
                2000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "later".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![Hop {
                        node_id: NodeId("n".into()),
                        channel: scid("1x1x0"),
                        amount_msat: 1000,
                        fee_msat: 0,
                        cltv_expiry: 0,
                        direction: None,
                    }],
                },
            ),
            make_envelope(
                "sender",
                1,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "earlier".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![Hop {
                        node_id: NodeId("n".into()),
                        channel: scid("1x1x0"),
                        amount_msat: 1000,
                        fee_msat: 0,
                        cltv_expiry: 0,
                        direction: None,
                    }],
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].payment_hash, "earlier");
        assert_eq!(traces[1].payment_hash, "later");
    }

    fn make_snapshot(spendable: u64) -> crate::ChannelSnapshot {
        crate::ChannelSnapshot {
            channel: scid("100x2x0"),
            peer: NodeId("node_c".into()),
            capacity_sat: 1_000_000,
            local_msat: spendable,
            remote_msat: 1_000_000_000u64.saturating_sub(spendable),
            active: true,
            direction: Some(0),
            state: Some("CHANNELD_NORMAL".into()),
            spendable_msat: Some(spendable),
            receivable_msat: Some(800_000_000),
            minimum_htlc_out_msat: Some(1000),
            max_accepted_htlcs: Some(483),
            inflight_htlc_count: Some(0),
        }
    }

    #[test]
    fn snapshot_enriches_failure_with_inferred_cause() {
        let envs = vec![
            // node_b's forward_event (local_failed) at seq=1
            make_envelope(
                "node_b",
                1,
                1000,
                TraceEvent::ForwardEvent {
                    in_channel: scid("100x1x0"),
                    out_channel: scid("100x2x0"),
                    in_msat: 50000501,
                    out_msat: 0,
                    fee_msat: 0,
                    status: ForwardStatus::LocalFailed,
                    payment_hash: Some("enrich_test".into()),
                    failcode: Some(4103),
                    failreason: Some("WIRE_TEMPORARY_CHANNEL_FAILURE".into()),
                },
            ),
            // Snapshot from node_b right after local_failed (seq=2, >= forward seq=1)
            make_envelope(
                "node_b",
                2,
                1001,
                TraceEvent::Snapshot {
                    channels: vec![make_snapshot(100_000)], // low spendable
                },
            ),
            // Sender's attempt
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "enrich_test".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: Some(1),
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: Some(0),
                        },
                    ],
                },
            ),
            // Sender's result — temporary_channel_failure at node_b
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "enrich_test".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x2x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.3),
                    failed_direction: Some(0),
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let a = &traces[0].attempts[0];
        let hop_fail = a.hops[1].failure.as_ref().unwrap();
        assert_eq!(hop_fail.sender_failcode, 4103);
        assert_eq!(
            hop_fail.local_failreason.as_deref(),
            Some("WIRE_TEMPORARY_CHANNEL_FAILURE")
        );
        let cause = hop_fail.inferred_cause.as_ref().unwrap();
        assert!(cause.contains("insufficient outbound liquidity"));
        assert!(cause.contains("100000 msat"));
        assert!(cause.contains("50000000 msat"));
    }

    #[test]
    fn later_healthy_snapshot_does_not_override_failure_cause() {
        // A later snapshot shows healthy liquidity (e.g. drain reversed),
        // but the cause should still use the snapshot taken right after the failure.
        let envs = vec![
            // node_b's forward_event (local_failed) at seq=1
            make_envelope(
                "node_b",
                1,
                1000,
                TraceEvent::ForwardEvent {
                    in_channel: scid("100x1x0"),
                    out_channel: scid("100x2x0"),
                    in_msat: 50000501,
                    out_msat: 0,
                    fee_msat: 0,
                    status: ForwardStatus::LocalFailed,
                    payment_hash: Some("override_test".into()),
                    failcode: Some(4103),
                    failreason: Some("WIRE_TEMPORARY_CHANNEL_FAILURE".into()),
                },
            ),
            // Snapshot from node_b right after failure (seq=2, low spendable)
            make_envelope(
                "node_b",
                2,
                1001,
                TraceEvent::Snapshot {
                    channels: vec![make_snapshot(100_000)],
                },
            ),
            // Later snapshot from node_b — healthy after drain reversed (seq=10)
            make_envelope(
                "node_b",
                10,
                5000,
                TraceEvent::Snapshot {
                    channels: vec![make_snapshot(500_000_000)], // plenty of liquidity
                },
            ),
            // Sender's attempt
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "override_test".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    route: vec![
                        Hop {
                            node_id: NodeId("node_b".into()),
                            channel: scid("100x1x0"),
                            amount_msat: 50000501,
                            fee_msat: 0,
                            cltv_expiry: 0,
                            direction: Some(1),
                        },
                        Hop {
                            node_id: NodeId("node_c".into()),
                            channel: scid("100x2x0"),
                            amount_msat: 50000000,
                            fee_msat: 501,
                            cltv_expiry: 0,
                            direction: Some(0),
                        },
                    ],
                },
            ),
            // Sender's result
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathResult {
                    payment_hash: "override_test".into(),
                    groupid: Some(1),
                    partid: Some(0),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x2x0")),
                    error_message: Some("temporary_channel_failure".into()),
                    duration_secs: Some(0.3),
                    failed_direction: Some(0),
                },
            ),
        ];

        let traces = correlate(&envs);
        let a = &traces[0].attempts[0];
        let hop_fail = a.hops[1].failure.as_ref().unwrap();

        // Must still say insufficient liquidity from the seq=2 snapshot,
        // NOT "unknown (healthy)" from the seq=10 snapshot.
        let cause = hop_fail.inferred_cause.as_ref().unwrap();
        assert!(
            cause.contains("insufficient outbound liquidity"),
            "expected insufficient liquidity but got: {cause}"
        );
        assert!(cause.contains("100000 msat"));
    }

    #[test]
    fn unmatched_start_with_final_outcome_is_cancelled() {
        // Shard 1 has start + result (failed), shard 2 has start only.
        // Since shard 1 has a result, the payment has a final outcome,
        // so shard 2 should be classified as Cancelled.
        let envs = vec![
            make_envelope(
                "sender",
                0,
                1000,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "cancel_test".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    route: vec![Hop {
                        node_id: NodeId("node_b".into()),
                        channel: scid("100x1x0"),
                        amount_msat: 50000,
                        fee_msat: 0,
                        cltv_expiry: 0,
                        direction: None,
                    }],
                },
            ),
            make_envelope(
                "sender",
                1,
                1001,
                TraceEvent::PaymentPathAttempt {
                    payment_hash: "cancel_test".into(),
                    groupid: Some(1),
                    partid: Some(2),
                    route: vec![Hop {
                        node_id: NodeId("node_c".into()),
                        channel: scid("100x2x0"),
                        amount_msat: 25000,
                        fee_msat: 0,
                        cltv_expiry: 0,
                        direction: None,
                    }],
                },
            ),
            make_envelope(
                "sender",
                2,
                1002,
                TraceEvent::PaymentPathResult {
                    payment_hash: "cancel_test".into(),
                    groupid: Some(1),
                    partid: Some(1),
                    success: false,
                    failcode: Some(4103),
                    erring_node: Some(NodeId("node_b".into())),
                    erring_channel: Some(scid("100x1x0")),
                    error_message: None,
                    duration_secs: Some(0.3),
                    failed_direction: None,
                },
            ),
        ];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let t = &traces[0];
        assert_eq!(t.attempts.len(), 2);

        let a1 = t.attempts.iter().find(|a| a.partid == Some(1)).unwrap();
        assert_eq!(a1.outcome, AttemptOutcome::Failed);
        assert!(a1.failure.is_some());

        let a2 = t.attempts.iter().find(|a| a.partid == Some(2)).unwrap();
        assert_eq!(a2.outcome, AttemptOutcome::Cancelled);
        assert!(a2.failure.is_none());
        assert!(a2.duration_secs.is_none());
    }

    #[test]
    fn solo_start_without_result_is_in_flight() {
        // A single PaymentPathAttempt with no result, no PaymentSent,
        // no PaymentFailed. The payment has no final outcome yet.
        let envs = vec![make_envelope(
            "sender",
            0,
            1000,
            TraceEvent::PaymentPathAttempt {
                payment_hash: "inflight_test".into(),
                groupid: Some(1),
                partid: Some(1),
                route: vec![Hop {
                    node_id: NodeId("node_b".into()),
                    channel: scid("100x1x0"),
                    amount_msat: 50000,
                    fee_msat: 0,
                    cltv_expiry: 0,
                    direction: None,
                }],
            },
        )];

        let traces = correlate(&envs);
        assert_eq!(traces.len(), 1);

        let t = &traces[0];
        assert_eq!(t.attempts.len(), 1);

        let a = &t.attempts[0];
        assert_eq!(a.outcome, AttemptOutcome::InFlight);
        assert!(a.failure.is_none());
        assert!(a.duration_secs.is_none());
    }
}
