//! Web UI: a single-page app embedded in the binary (no JS build step).
//! Set `DS_WEB_DIR=crates/dataset-server/web` to serve from disk while editing.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::AppState;

const INDEX: &str = include_str!("../web/index.html");
const CSS: &str = include_str!("../web/app.css");
const JS: &str = include_str!("../web/app.js");

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(index)).route("/assets/{file}", get(asset))
}

fn load(name: &str, embedded: &'static str) -> String {
    match std::env::var_os("DS_WEB_DIR") {
        Some(dir) => std::fs::read_to_string(std::path::Path::new(&dir).join(name)).unwrap_or_else(|_| embedded.to_string()),
        None => embedded.to_string(),
    }
}

fn respond(content_type: &'static str, body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'",
            ),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    respond("text/html; charset=utf-8", load("index.html", INDEX))
}

async fn asset(Path(file): Path<String>) -> Response {
    match file.as_str() {
        "app.css" => respond("text/css; charset=utf-8", load("app.css", CSS)),
        "app.js" => respond("text/javascript; charset=utf-8", load("app.js", JS)),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
