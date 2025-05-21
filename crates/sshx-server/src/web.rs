//! HTTP and WebSocket handlers for the sshx web interface.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{any, get_service};
use axum::{Json, Router};
use tower_http::services::{ServeDir, ServeFile};

use crate::ServerState;

pub mod protocol;
mod socket;

/// Returns the web application server, routed with Axum.
pub fn app(state: &Arc<ServerState>) -> Router<Arc<ServerState>> {
    let root_spa = ServeFile::new("build/spa.html")
        .precompressed_gzip()
        .precompressed_br();

    // Serves static SvelteKit build files.
    let static_files = ServeDir::new("build")
        .precompressed_gzip()
        .precompressed_br()
        .fallback(root_spa);

    Router::new()
        .nest("/api", backend())
        .fallback_service(get_service(static_files))
        .with_state(state.clone())
}

/// Routes for the backend web API server.
fn backend() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/s/{name}", any(socket::get_session_ws))
        .route("/session/list", any(get_session_list))
    // Json(state.get_all_session_names())
}
async fn get_session_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth_str = headers.get("Authorization");
    // let s = Vec::<String>::new();
    if auth_str.unwrap().is_empty() {
        return Json(Vec::<String>::new());
    }
    if auth_str.unwrap().to_str().unwrap_or_default() != state.secret {
        return Json(Vec::<String>::new());
    }
    let session_names = state.get_all_session_names();
    Json(session_names)
}
