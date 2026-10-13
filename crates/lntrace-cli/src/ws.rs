//! WebSocket handler for live mode.

use crate::live::{LiveState, StateUpdate};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Combined state for the live-mode router.
#[derive(Clone)]
pub struct LiveAppState {
    pub live: Arc<RwLock<LiveState>>,
    pub update_tx: broadcast::Sender<StateUpdate>,
}

/// Axum handler: upgrade HTTP to WebSocket.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<LiveAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Snapshot message sent on initial connect.
#[derive(Serialize)]
struct SnapshotMsg<'a> {
    r#type: &'a str,
    graph: &'a lntrace::GraphResponse,
    traces: &'a [lntrace::TraceListEntry],
}

/// Update message sent when state changes.
#[derive(Serialize)]
struct UpdateMsg<'a> {
    r#type: &'a str,
    traces: &'a [lntrace::TraceListEntry],
    changed: &'a [String],
    topology_changed: bool,
    /// Included only when `topology_changed` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<&'a lntrace::GraphResponse>,
}

/// Serialize and send a snapshot message. Returns `Err(())` on failure.
async fn send_snapshot(socket: &mut WebSocket, state: &LiveState) -> Result<(), ()> {
    let json = serde_json::to_string(&SnapshotMsg {
        r#type: "snapshot",
        graph: &state.graph,
        traces: &state.trace_list,
    })
    .map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

/// Serialize and send an update message. Returns `Err(())` on failure.
async fn send_update(
    socket: &mut WebSocket,
    live: &Arc<RwLock<LiveState>>,
    update: &StateUpdate,
) -> Result<(), ()> {
    let graph_data;
    let graph_ref = if update.topology_changed {
        let s = live.read().await;
        graph_data = s.graph.clone();
        Some(&graph_data)
    } else {
        None
    };
    let json = serde_json::to_string(&UpdateMsg {
        r#type: "update",
        traces: &update.trace_list,
        changed: &update.changed_hashes,
        topology_changed: update.topology_changed,
        graph: graph_ref,
    })
    .map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

async fn handle_socket(mut socket: WebSocket, state: Arc<LiveAppState>) {
    // Subscribe to updates BEFORE reading the snapshot to avoid missing any.
    let mut update_rx = state.update_tx.subscribe();

    // Send initial snapshot.
    {
        let s = state.live.read().await;
        if send_snapshot(&mut socket, &s).await.is_err() {
            return;
        }
    }

    // Drain any updates that arrived between subscribe and snapshot send.
    while let Ok(update) = update_rx.try_recv() {
        if send_update(&mut socket, &state.live, &update)
            .await
            .is_err()
        {
            return;
        }
    }

    // Main loop: forward broadcast updates to the client.
    loop {
        tokio::select! {
            result = update_rx.recv() => {
                match result {
                    Ok(update) => {
                        if send_update(&mut socket, &state.live, &update).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Client fell behind — send a fresh snapshot.
                        let s = state.live.read().await;
                        if send_snapshot(&mut socket, &s).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // Ignore client messages.
                }
            }
        }
    }
}
