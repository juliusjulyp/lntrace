//! Axum web server for the lntrace graph UI.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use lntrace::{GraphResponse, Trace, TraceListEntry};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

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
