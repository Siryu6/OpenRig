//! The web app, built into the program (web/ in the source tree): plain
//! HTML, CSS and JavaScript, no build step.

use axum::http::header;
use axum::response::IntoResponse;

const INDEX: &str = include_str!("../../web/index.html");
const SCRIPT: &str = include_str!("../../web/app.js");
const STYLE: &str = include_str!("../../web/app.css");
const ICON: &str = include_str!("../../web/icon.svg");
const MANIFEST: &str = include_str!("../../web/manifest.webmanifest");

pub async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX)
}

pub async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        SCRIPT,
    )
}

pub async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

pub async fn icon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], ICON)
}

pub async fn manifest() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        MANIFEST,
    )
}
