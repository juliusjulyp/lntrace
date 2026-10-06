//! Live state manager: receives tail batches, re-correlates, broadcasts
//! updates to WebSocket clients.

use crate::tailer::TailBatch;
use lntrace::{
    build_graph, build_trace_list, correlate, Envelope, GraphResponse, Trace, TraceListEntry,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

/// Shared mutable state for live mode.
pub struct LiveState {
    pub envelopes: Vec<Envelope>,
    pub aliases: HashMap<String, String>,
    pub graph: GraphResponse,
    pub traces: Vec<Trace>,
    pub trace_list: Vec<TraceListEntry>,
}

/// Broadcast to WebSocket clients when state changes.
#[derive(Clone, Debug, Serialize)]
pub struct StateUpdate {
    pub trace_list: Vec<TraceListEntry>,
    pub changed_hashes: Vec<String>,
    /// True when the node or channel set changed (client should re-layout).
    pub topology_changed: bool,
}

/// Run the state manager loop.
///
/// Receives [`TailBatch`] from the directory tailer, debounces to at most one
/// re-correlation per `debounce_ms`, and announces [`StateUpdate`] on the
/// broadcast channel.
pub async fn run_state_manager(
    state: Arc<RwLock<LiveState>>,
    mut rx: mpsc::Receiver<TailBatch>,
    update_tx: broadcast::Sender<StateUpdate>,
    debounce_ms: u64,
) {
    loop {
        // Block until the first batch arrives.
        let batch = match rx.recv().await {
            Some(b) => b,
            None => break, // channel closed
        };

        // Collect this batch plus anything else that arrived.
        let mut new_envelopes = batch.envelopes;
        let mut new_aliases = batch.aliases;

        // Debounce: wait a bit, then drain any additional batches.
        tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
        while let Ok(more) = rx.try_recv() {
            new_envelopes.extend(more.envelopes);
            for (k, v) in more.aliases {
                new_aliases.entry(k).or_insert(v);
            }
        }

        // Snapshot old state for diffing.
        let (old_fingerprints, old_node_ids, old_scids) = {
            let s = state.read().await;
            let fps: HashSet<(String, usize, bool)> = s
                .traces
                .iter()
                .map(|t| (t.payment_hash.clone(), t.attempts.len(), t.success))
                .collect();
            let nodes: HashSet<String> = s.graph.nodes.iter().map(|n| n.id.clone()).collect();
            let scids: HashSet<String> = s.graph.channels.iter().map(|c| c.scid.clone()).collect();
            (fps, nodes, scids)
        };

        // Write-lock: append new data, re-derive everything.
        let update = {
            let mut s = state.write().await;
            s.envelopes.extend(new_envelopes);
            for (k, v) in new_aliases {
                s.aliases.entry(k).or_insert(v);
            }
            s.envelopes.sort_by_key(|e| e.node_ts_ms);

            s.traces = correlate(&s.envelopes);
            s.graph = build_graph(&s.envelopes, &s.aliases);
            s.trace_list = build_trace_list(&s.traces);

            // Diff: find changed or new traces.
            let changed_hashes: Vec<String> = s
                .traces
                .iter()
                .filter(|t| {
                    !old_fingerprints.contains(&(
                        t.payment_hash.clone(),
                        t.attempts.len(),
                        t.success,
                    ))
                })
                .map(|t| t.payment_hash.clone())
                .collect();

            // Detect topology changes (new/removed nodes or channels).
            let new_node_ids: HashSet<String> =
                s.graph.nodes.iter().map(|n| n.id.clone()).collect();
            let new_scids: HashSet<String> =
                s.graph.channels.iter().map(|c| c.scid.clone()).collect();
            let topology_changed = new_node_ids != old_node_ids || new_scids != old_scids;

            StateUpdate {
                trace_list: s.trace_list.clone(),
                changed_hashes,
                topology_changed,
            }
        };

        // Broadcast (ignore error = no receivers).
        let _ = update_tx.send(update);
    }
}
