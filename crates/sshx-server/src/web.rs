//! HTTP and WebSocket handlers for the sshx web interface.

use std::sync::Arc;

use axum::routing::{any, get_service};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::limit::RequestBodyLimitLayer;
use axum::extract::DefaultBodyLimit;

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
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024 * 1024)) // 4GB limit
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024 * 1024)) // 4GB limit for multipart
        .with_state(state.clone())
}

/// Routes for the backend web API server.
fn backend() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/s/{name}", any(socket::get_session_ws))
        .route("/session/list", any(socket::get_session_list))
        .route("/files/list/{*path}", any(socket::list_directory))
        .route("/files/upload", any(socket::upload_file))
        .route("/files/download/{*path}", any(socket::download_file))
    // Json(state.get_all_session_names())
}
