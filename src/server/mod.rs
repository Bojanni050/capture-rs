//! Lokale webserver: JSON-API plus een UI om je dag terug te kijken.
//!
//! Bindt standaard alleen op 127.0.0.1. Er zit bewust geen authenticatie in;
//! de aanname is dat dit op je eigen machine draait en niet naar buiten wordt
//! opengezet. Zet je `bind` op een extern adres, doe dat dan achter een proxy
//! die de toegang regelt.

mod ui;

use crate::store::{Db, FrameStore, SearchQuery};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub frames: Arc<FrameStore>,
}

/// Fouten netjes als JSON teruggeven in plaats van een kale 500.
struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(error = %self.0, "API-fout");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/search", get(search))
        .route("/api/timeline", get(timeline))
        .route("/api/stats", get(stats))
        .route("/api/apps", get(apps))
        .route("/api/capture/{id}", get(capture))
        .route("/api/frame/{id}", get(frame))
        .with_state(state)
}

pub async fn serve(state: AppState, bind: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = format!("{bind}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = router(state);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "webserver gestopt");
        }
    });

    Ok(local)
}

async fn index() -> Html<&'static str> {
    Html(ui::PAGE)
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
    app: Option<String>,
    kind: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn search(
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let query = SearchQuery {
        text: p.q,
        app: p.app.filter(|a| !a.is_empty()),
        kind: p.kind.filter(|k| !k.is_empty()),
        from: p.from,
        to: p.to,
        limit: p.limit.unwrap_or(50).clamp(1, 500),
        offset: p.offset.unwrap_or(0).max(0),
    };
    let hits = state.db.search(&query)?;
    Ok(Json(json!({ "hits": hits, "count": hits.len() })))
}

#[derive(Debug, Deserialize)]
struct RangeParams {
    from: Option<i64>,
    to: Option<i64>,
}

impl RangeParams {
    /// Standaard: de afgelopen 24 uur.
    fn resolve(&self) -> (i64, i64) {
        let now = chrono::Local::now().timestamp();
        (self.from.unwrap_or(now - 86_400), self.to.unwrap_or(now))
    }
}

async fn timeline(
    State(state): State<AppState>,
    Query(p): Query<RangeParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let (from, to) = p.resolve();
    let segments = state.db.timeline(from, to)?;
    Ok(Json(json!({ "segments": segments })))
}

async fn stats(
    State(state): State<AppState>,
    Query(p): Query<RangeParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let (from, to) = p.resolve();
    let stats = state.db.stats(from, to)?;
    let bytes = state.frames.disk_usage();
    Ok(Json(json!({ "stats": stats, "frame_bytes": bytes })))
}

async fn apps(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({ "apps": state.db.apps()? })))
}

async fn capture(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    match state.db.capture(id)? {
        Some(c) => Ok(Json(c).into_response()),
        None => Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "niet gevonden" }))).into_response()),
    }
}

async fn frame(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let Some(path) = state.db.frame_path(id)? else {
        return Ok((StatusCode::NOT_FOUND, "geen frame bij deze capture").into_response());
    };
    let bytes = state.frames.read(&path)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            // Frames zijn onveranderlijk zolang ze bestaan.
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}
