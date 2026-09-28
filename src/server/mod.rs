//! Lokale webserver: JSON-API plus een UI om je dag terug te kijken.
//!
//! Bindt standaard alleen op 127.0.0.1. Er zit bewust geen authenticatie in;
//! de aanname is dat dit op je eigen machine draait en niet naar buiten wordt
//! opengezet. Zet je `bind` op een extern adres, doe dat dan achter een proxy
//! die de toegang regelt.

mod ui;

use crate::embeddings::{store::VectorStore, EmbeddingProvider};
use crate::logs::LogBuffer;
use crate::store::{Db, FrameStore, SearchQuery};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub frames: Arc<FrameStore>,
    pub embeddings_store: Option<Arc<dyn VectorStore>>,
    pub embeddings_provider: Option<Arc<dyn EmbeddingProvider>>,
    pub status: SharedStatus,
    pub logs: LogBuffer,
}

/// Momentopname van het draaiende proces voor de webinterface: alles wat je
/// anders in de terminal zou aflezen bij het opstarten (webinterface-adres,
/// bridge, shipper, UIA, OCR, schermen) plus wat er sindsdien gebeurde.
#[derive(Debug, Clone, Serialize)]
pub struct SystemStatus {
    pub version: String,
    pub started_at: i64,
    /// true bij `start`, false bij `serve` (alleen webinterface, geen opname).
    pub recording: bool,
    pub server_bind: String,
    pub server_port: u16,
    pub browser_enabled: bool,
    pub browser_port: u16,
    /// Of de bridge echt gestart is (kan mislukken als de poort bezet is).
    pub browser_running: bool,
    pub ship_enabled: bool,
    pub ship_endpoint: String,
    pub ship_total: u64,
    pub ship_last_count: Option<usize>,
    pub ship_last_at: Option<i64>,
    pub uia_enabled: bool,
    /// Of UIA echt gestart is (niet alleen aangevinkt in de config).
    pub uia_active: bool,
    pub ocr_enabled: bool,
    pub ocr_language: Option<String>,
    pub monitors: String,
    /// None = de opname is nog niet ver genoeg om het te weten.
    pub foreground_events: Option<bool>,
    pub autostart: bool,
    pub console_hidden: bool,
}

impl SystemStatus {
    pub fn new(cfg: &crate::config::Config, recording: bool) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: Local::now().timestamp(),
            recording,
            server_bind: cfg.server.bind.clone(),
            server_port: cfg.server.port,
            browser_enabled: cfg.browser.enabled,
            browser_port: cfg.browser.port,
            browser_running: false,
            ship_enabled: cfg.ship.enabled,
            ship_endpoint: cfg.ship.endpoint.clone(),
            ship_total: 0,
            ship_last_count: None,
            ship_last_at: None,
            uia_enabled: cfg.uia.enabled,
            uia_active: false,
            ocr_enabled: cfg.ocr.enabled,
            ocr_language: None,
            monitors: String::new(),
            foreground_events: None,
            autostart: crate::autostart::is_enabled(),
            console_hidden: false,
        }
    }
}

pub type SharedStatus = Arc<RwLock<SystemStatus>>;

fn lock(status: &SharedStatus) -> std::sync::RwLockWriteGuard<'_, SystemStatus> {
    // Zelfde redenatie als `Db::lock`: een vergiftigde mutex betekent een
    // paniek elders; de laatst bekende status tonen is beter dan crashen.
    status.write().unwrap_or_else(|e| e.into_inner())
}

pub fn set_browser_running(status: &SharedStatus, running: bool) {
    lock(status).browser_running = running;
}

pub fn set_console_hidden(status: &SharedStatus, hidden: bool) {
    lock(status).console_hidden = hidden;
}

/// Wat de pipeline bij het opstarten echt aantrof (tegenover de config).
pub fn set_pipeline_info(
    status: &SharedStatus,
    uia_active: bool,
    ocr_language: Option<String>,
    monitors: String,
) {
    let mut s = lock(status);
    s.uia_active = uia_active;
    s.ocr_language = ocr_language;
    s.monitors = monitors;
}

pub fn set_foreground_events(status: &SharedStatus, active: bool) {
    lock(status).foreground_events = Some(active);
}

/// Houdt bij wat de shipper verzond, voor de "naar Foundation Gateway
/// verzonden"-regel uit de terminal.
pub fn note_shipped(status: &SharedStatus, count: usize) {
    let mut s = lock(status);
    s.ship_total += count as u64;
    s.ship_last_count = Some(count);
    s.ship_last_at = Some(Local::now().timestamp());
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
        .route("/api/status", get(status))
        .route("/api/logs", get(logs))
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

/// Dezelfde regels als in de terminal bij het opstarten, maar dan als staat.
async fn status(State(state): State<AppState>) -> ApiResult<Json<SystemStatus>> {
    let snapshot = state
        .status
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Ok(Json(snapshot))
}

/// De laatste logregels uit het geheugen (zie `logs.rs`).
async fn logs(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({ "logs": state.logs.recent() }))
}
