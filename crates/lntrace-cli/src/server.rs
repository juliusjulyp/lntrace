//! Axum web server for the lntrace graph UI.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use lntrace::{GraphResponse, Trace, TraceListEntry};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::ws::{self, LiveAppState};

// ---------------------------------------------------------------------------
// Static mode (no --follow)
// ---------------------------------------------------------------------------

pub struct AppState {
    pub graph: GraphResponse,
    pub traces: Vec<Trace>,
    pub trace_list: Vec<TraceListEntry>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/graph", get(get_graph))
        .route("/api/traces", get(get_traces))
        .route("/api/traces/{hash}", get(get_trace))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn get_graph(State(state): State<Arc<AppState>>) -> Json<GraphResponse> {
    Json(state.graph.clone())
}

async fn get_traces(State(state): State<Arc<AppState>>) -> Json<Vec<TraceListEntry>> {
    Json(state.trace_list.clone())
}

async fn get_trace(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> Result<Json<Trace>, StatusCode> {
    state
        .traces
        .iter()
        .find(|t| t.payment_hash == hash)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

// ---------------------------------------------------------------------------
// Live mode (--follow)
// ---------------------------------------------------------------------------

pub fn build_live_router(state: LiveAppState) -> Router {
    Router::new()
        .route("/", get(live_index))
        .route("/api/graph", get(get_graph_live))
        .route("/api/traces", get(get_traces_live))
        .route("/api/traces/{hash}", get(get_trace_live))
        .route("/ws", get(ws::ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state))
}

async fn live_index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn get_graph_live(State(state): State<Arc<LiveAppState>>) -> Json<GraphResponse> {
    let s = state.live.read().await;
    Json(s.graph.clone())
}

async fn get_traces_live(State(state): State<Arc<LiveAppState>>) -> Json<Vec<TraceListEntry>> {
    let s = state.live.read().await;
    Json(s.trace_list.clone())
}

async fn get_trace_live(
    State(state): State<Arc<LiveAppState>>,
    Path(hash): Path<String>,
) -> Result<Json<Trace>, StatusCode> {
    let s = state.live.read().await;
    s.traces
        .iter()
        .find(|t| t.payment_hash == hash)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
