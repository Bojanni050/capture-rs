//! Lokale webserver: JSON-API plus een UI om je dag terug te kijken.
//!
//! Bindt standaard alleen op 127.0.0.1. Er zit bewust geen authenticatie in;
//! de aanname is dat dit op je eigen machine draait en niet naar buiten wordt
//! opengezet. Zet je `bind` op een extern adres, doe dat dan achter een proxy
//! die de toegang regelt.

mod ui;

use crate::embeddings::{store::VectorStore, EmbeddingProvider};
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
    pub embeddings_store: Option<Arc<dyn VectorStore>>,
    pub embeddings_provider: Option<Arc<dyn EmbeddingProvider>>,
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
        .route("/api/search/semantic", get(semantic_search))
        .route("/api/timeline", get(timeline))
        .route("/api/stats", get(stats))
        .route("/api/apps", get(apps))
        .route("/api/capture/{id}", get(capture))
        .route("/api/frame/{id}", get(frame))
        .route("/api/embeddings/status", get(embeddings_status))
        .with_state(state)
}

pub async fn serve(state: AppState, bind: &str, port: u16) -> anyhow::Result<SocketAddr> {
    if bind != "127.0.0.1" && bind != "localhost" && bind != "::1" {
        tracing::warn!(
            bind = bind,
            "webserver bindt niet op localhost — geen authenticatie, alleen achter proxy blootstellen!"
        );
    }
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
    let db = Arc::clone(&state.db);
    let hits = tokio::task::spawn_blocking(move || db.search(&query))
        .await
        .map_err(|e| anyhow::anyhow!("search task panicked: {e}"))??;
    Ok(Json(json!({ "hits": hits, "count": hits.len() })))
}

#[derive(Debug, Deserialize)]
struct RangeParams {
    from: Option<i64>,
    to: Option<i64>,
}

impl RangeParams {
    /// Standaard: de afgelopen 24 uur (UTC epoch, zelfde als Local.timestamp()).
    fn resolve(&self) -> (i64, i64) {
        let now = chrono::Utc::now().timestamp();
        (self.from.unwrap_or(now - 86_400), self.to.unwrap_or(now))
    }
}

async fn timeline(
    State(state): State<AppState>,
    Query(p): Query<RangeParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let (from, to) = p.resolve();
    let db = Arc::clone(&state.db);
    let segments = tokio::task::spawn_blocking(move || db.timeline(from, to))
        .await
        .map_err(|e| anyhow::anyhow!("timeline task panicked: {e}"))??;
    Ok(Json(json!({ "segments": segments })))
}

async fn stats(
    State(state): State<AppState>,
    Query(p): Query<RangeParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let (from, to) = p.resolve();
    let db = Arc::clone(&state.db);
    let frames = Arc::clone(&state.frames);
    let (stats, bytes) = tokio::task::spawn_blocking(move || {
        let stats = db.stats(from, to)?;
        let bytes = frames.disk_usage();
        Ok::<_, anyhow::Error>((stats, bytes))
    })
    .await
    .map_err(|e| anyhow::anyhow!("stats task panicked: {e}"))??;
    Ok(Json(json!({ "stats": stats, "frame_bytes": bytes })))
}

async fn apps(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let db = Arc::clone(&state.db);
    let apps = tokio::task::spawn_blocking(move || db.apps())
        .await
        .map_err(|e| anyhow::anyhow!("apps task panicked: {e}"))??;
    Ok(Json(json!({ "apps": apps })))
}

async fn capture(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let db = Arc::clone(&state.db);
    let cap = tokio::task::spawn_blocking(move || db.capture(id))
        .await
        .map_err(|e| anyhow::anyhow!("capture task panicked: {e}"))??;
    match cap {
        Some(c) => Ok(Json(c).into_response()),
        None => Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "niet gevonden" }))).into_response()),
    }
}

async fn frame(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let db = Arc::clone(&state.db);
    let frames = Arc::clone(&state.frames);
    let result = tokio::task::spawn_blocking(move || {
        let path = db.frame_path(id)?;
        let Some(path) = path else {
            return Ok::<_, anyhow::Error>(None);
        };
        let bytes = frames.read(&path)?;
        Ok(Some(bytes))
    })
    .await
    .map_err(|e| anyhow::anyhow!("frame task panicked: {e}"))??;
    let Some(bytes) = result else {
        return Ok((StatusCode::NOT_FOUND, "geen frame bij deze capture").into_response());
    };
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

#[derive(Debug, Deserialize)]
struct SemanticSearchParams {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

async fn semantic_search(
    State(state): State<AppState>,
    Query(p): Query<SemanticSearchParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(store) = state.embeddings_store else {
        return Ok(Json(json!({"error": "embeddings uitgeschakeld", "hits": []})));
    };
    let Some(provider) = state.embeddings_provider else {
        return Ok(Json(json!({"error": "geen embedding provider", "hits": []})));
    };
    if p.q.trim().is_empty() {
        return Ok(Json(json!({"hits": [], "count": 0})));
    }
    let limit = p.limit.unwrap_or(20).clamp(1, 100);
    let q = p.q.clone();
    let emb = tokio::task::spawn_blocking(move || provider.embed(&[q]))
        .await
        .map_err(|e| anyhow::anyhow!("embed panicked: {e}"))??;
    let hits = store.search(emb.into_iter().next().unwrap(), limit).await?;
    Ok(Json(json!({"hits": hits, "count": hits.len()})))
}

async fn embeddings_status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let Some(store) = state.embeddings_store else {
        return Ok(Json(json!({"enabled": false, "count": 0})));
    };
    match store.count().await {
        Ok(n) => Ok(Json(json!({"enabled": true, "count": n, "available": store.is_available()}))),
        Err(e) => Ok(Json(json!({"enabled": true, "error": e.to_string()}))),
    }
}
