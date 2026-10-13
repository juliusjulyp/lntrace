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
// TraceData trait: unified data access for static and live modes
// ---------------------------------------------------------------------------

trait TraceData: Clone + Send + Sync + 'static {
    fn graph(&self) -> impl std::future::Future<Output = GraphResponse> + Send;
    fn traces(&self) -> impl std::future::Future<Output = Vec<Trace>> + Send;
    fn trace_list(&self) -> impl std::future::Future<Output = Vec<TraceListEntry>> + Send;
}

// ---------------------------------------------------------------------------
// Static mode (no --follow)
// ---------------------------------------------------------------------------

pub struct AppState {
    pub graph: GraphResponse,
    pub traces: Vec<Trace>,
    pub trace_list: Vec<TraceListEntry>,
}

impl TraceData for Arc<AppState> {
    async fn graph(&self) -> GraphResponse {
        self.graph.clone()
    }
    async fn traces(&self) -> Vec<Trace> {
        self.traces.clone()
    }
    async fn trace_list(&self) -> Vec<TraceListEntry> {
        self.trace_list.clone()
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/graph", get(get_graph::<Arc<AppState>>))
        .route("/api/traces", get(get_traces::<Arc<AppState>>))
        .route("/api/traces/{hash}", get(get_trace::<Arc<AppState>>))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Live mode (--follow)
// ---------------------------------------------------------------------------

impl TraceData for Arc<LiveAppState> {
    async fn graph(&self) -> GraphResponse {
        self.live.read().await.graph.clone()
    }
    async fn traces(&self) -> Vec<Trace> {
        self.live.read().await.traces.clone()
    }
    async fn trace_list(&self) -> Vec<TraceListEntry> {
        self.live.read().await.trace_list.clone()
    }
}

pub fn build_live_router(state: LiveAppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/graph", get(get_graph::<Arc<LiveAppState>>))
        .route("/api/traces", get(get_traces::<Arc<LiveAppState>>))
        .route("/api/traces/{hash}", get(get_trace::<Arc<LiveAppState>>))
        .route("/ws", get(ws::ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state))
}

// ---------------------------------------------------------------------------
// Shared handlers
// ---------------------------------------------------------------------------

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn get_graph<T: TraceData>(State(state): State<T>) -> Json<GraphResponse> {
    Json(state.graph().await)
}

async fn get_traces<T: TraceData>(State(state): State<T>) -> Json<Vec<TraceListEntry>> {
    Json(state.trace_list().await)
}

async fn get_trace<T: TraceData>(
    State(state): State<T>,
    Path(hash): Path<String>,
) -> Result<Json<Trace>, StatusCode> {
    state
        .traces()
        .await
        .into_iter()
        .find(|t| t.payment_hash == hash)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
