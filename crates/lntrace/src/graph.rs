//! Build a graph view and trace summaries from correlated envelopes.
//!
//! Pure functions: envelopes/traces in, response structs out, no I/O.

use crate::{correlator::Trace, Envelope, TraceEvent};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Graph response for the UI: nodes and channels with balances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphResponse {
    pub nodes: Vec<GraphNode>,
    pub channels: Vec<GraphChannel>,
}

/// A node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Whether this node runs the lntrace adapter (submits events).
    /// False for nodes known only as channel peers.
    pub instrumented: bool,
}

/// A channel in the graph, with balances from both sides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphChannel {
    /// Short channel id in BxTxO format.
    pub scid: String,
    /// Lexicographically lower node_id.
    pub node_a: String,
    /// Lexicographically higher node_id.
    pub node_b: String,
    pub capacity_sat: u64,
    /// node_a's local balance (to_us_msat from a's perspective).
    pub a_local_msat: u64,
    /// node_b's local balance (to_us_msat from b's perspective).
    pub b_local_msat: u64,
}

/// Summary of a trace for list views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceListEntry {
    pub payment_hash: String,
    pub amount_msat: u64,
    pub success: bool,
    pub attempt_count: usize,
}

/// Build a graph from envelopes and an alias map.
///
/// Extracts nodes from all envelope `node_id` fields and channels from
/// the latest `Snapshot` event per node. When two nodes report the same
/// scid, their perspectives are merged into one `GraphChannel`.
pub fn build_graph(envelopes: &[Envelope], aliases: &HashMap<String, String>) -> GraphResponse {
    // Collect instrumented node_ids (nodes that submitted envelopes).
    let instrumented_ids: HashSet<String> = envelopes.iter().map(|e| e.node_id.0.clone()).collect();

    // Also collect peer node_ids from channel snapshots.
    let mut all_node_ids: HashSet<String> = instrumented_ids.clone();
    for env in envelopes {
        if let TraceEvent::Snapshot { channels } = &env.event {
            for ch in channels {
                all_node_ids.insert(ch.peer.0.clone());
            }
        }
    }

    let mut node_ids: Vec<String> = all_node_ids.into_iter().collect();
    node_ids.sort();

    let nodes: Vec<GraphNode> = node_ids
        .iter()
        .map(|id| GraphNode {
            id: id.clone(),
            alias: aliases.get(id).cloned(),
            instrumented: instrumented_ids.contains(id),
        })
        .collect();

    // For each node, find its latest Snapshot envelope.
    let mut latest_snapshots: HashMap<&str, &Envelope> = HashMap::new();
    for env in envelopes {
        if matches!(&env.event, TraceEvent::Snapshot { .. }) {
            let existing = latest_snapshots.get(env.node_id.0.as_str());
            if existing.map_or(true, |prev| env.node_ts_ms >= prev.node_ts_ms) {
                latest_snapshots.insert(&env.node_id.0, env);
            }
        }
    }

    // Build channels keyed by scid. Each side records its local_msat.
    struct ChannelSide {
        node_id: String,
        peer_id: String,
        local_msat: u64,
        capacity_sat: u64,
    }

    let mut channel_map: HashMap<String, Vec<ChannelSide>> = HashMap::new();

    for env in latest_snapshots.values() {
        let channels = match &env.event {
            TraceEvent::Snapshot { channels } => channels,
            _ => continue,
        };
        for ch in channels {
            let scid = match ch.channel.scid {
                Some(s) => s.to_string(),
                None => continue,
            };
            let sides = channel_map.entry(scid).or_default();
            // Avoid duplicate sides from the same node.
            if !sides.iter().any(|s| s.node_id == env.node_id.0) {
                sides.push(ChannelSide {
                    node_id: env.node_id.0.clone(),
                    peer_id: ch.peer.0.clone(),
                    local_msat: ch.local_msat,
                    capacity_sat: ch.capacity_sat,
                });
            }
        }
    }

    let mut channels: Vec<GraphChannel> = channel_map
        .into_iter()
        .map(|(scid, sides)| {
            // Canonicalize: node_a is lexicographically lower.
            let (a_id, a_local, b_id, b_local, capacity_sat);

            if sides.len() >= 2 {
                // Both sides reported.
                let (s0, s1) = (&sides[0], &sides[1]);
                capacity_sat = s0.capacity_sat;
                if s0.node_id < s1.node_id {
                    a_id = s0.node_id.clone();
                    a_local = s0.local_msat;
                    b_id = s1.node_id.clone();
                    b_local = s1.local_msat;
                } else {
                    a_id = s1.node_id.clone();
                    a_local = s1.local_msat;
                    b_id = s0.node_id.clone();
                    b_local = s0.local_msat;
                }
            } else if sides.len() == 1 {
                // One side reported — use peer_id for the other end.
                let s = &sides[0];
                capacity_sat = s.capacity_sat;
                let remote = (s.capacity_sat * 1000).saturating_sub(s.local_msat);
                if s.node_id < s.peer_id {
                    a_id = s.node_id.clone();
                    a_local = s.local_msat;
                    b_id = s.peer_id.clone();
                    b_local = remote;
                } else {
                    a_id = s.peer_id.clone();
                    a_local = remote;
                    b_id = s.node_id.clone();
                    b_local = s.local_msat;
                }
            } else {
                // No sides — shouldn't happen but handle gracefully.
                a_id = String::new();
                a_local = 0;
                b_id = String::new();
                b_local = 0;
                capacity_sat = 0;
            }

            GraphChannel {
                scid,
                node_a: a_id,
                node_b: b_id,
                capacity_sat,
                a_local_msat: a_local,
                b_local_msat: b_local,
            }
        })
        .collect();

    // Sort channels by scid for deterministic output.
    channels.sort_by(|a, b| a.scid.cmp(&b.scid));

    GraphResponse { nodes, channels }
}

/// Summarize traces for a list view.
pub fn build_trace_list(traces: &[Trace]) -> Vec<TraceListEntry> {
    traces
        .iter()
        .map(|t| {
            let amount_msat = t
                .attempts
                .first()
                .and_then(|a| a.hops.last())
                .map(|h| h.amount_msat)
                .unwrap_or(0);

            TraceListEntry {
                payment_hash: t.payment_hash.clone(),
                amount_msat,
                success: t.success,
                attempt_count: t.attempts.len(),
            }
        })
        .collect()
}
