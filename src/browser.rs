//! Brug naar de browserextensie (`browser-extension/`).
//!
//! UIA weet niet betrouwbaar op welke site je zit, en ziet een wachtwoordveld
//! alleen op het moment dat het in beeld is. De extensie kijkt in de DOM en
//! meldt hier de hostnaam van het actieve tabblad plus of er een
//! `input[type=password]` staat. Daarmee kan de pipeline een héél domein
//! uitsluiten, ook op pagina's van die site zonder loginformulier.
//!
//! Alleen bereikbaar op 127.0.0.1, en alleen voor extensies: een gewone
//! webpagina die hier naartoe post (CORS geldt niet voor "simpele" requests)
//! zou anders elk domein kunnen uitsluiten of zich als een ander domein
//! kunnen voordoen. Browsers sturen altijd een `Origin`-header mee, dus we
//! weigeren alles wat niet van een extensie komt.

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Report {
    hostname: String,
    has_password_field: bool,
    at: Instant,
}

/// Laatste melding van de extensie.
#[derive(Default)]
pub struct BrowserState {
    latest: Mutex<Option<Report>>,
}

impl BrowserState {
    pub fn update(&self, hostname: &str, has_password_field: bool) {
        let hostname = hostname.trim().to_lowercase();
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        // Een pagina zonder hostnaam (nieuw tabblad, about:blank) mag de
        // vorige site niet laten doorlopen.
        *latest = (!hostname.is_empty()).then(|| Report {
            hostname,
            has_password_field,
            at: Instant::now(),
        });
    }

    /// `(hostnaam, heeft_wachtwoordveld)`, of `None` als we nooit iets hoorden
    /// of de laatste melding te oud is om te vertrouwen.
    pub fn current(&self, max_age: Duration) -> Option<(String, bool)> {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        let report = latest.as_ref()?;
        (report.at.elapsed() <= max_age)
            .then(|| (report.hostname.clone(), report.has_password_field))
    }
}

#[derive(Deserialize)]
struct Payload {
    hostname: String,
    #[serde(default, rename = "hasPasswordField")]
    has_password_field: bool,
}

/// `Ok(Some(origin))` voor een extensie, `Ok(None)` zonder Origin (curl,
/// lokale tooling), `Err` voor al het andere.
fn check_origin(headers: &HeaderMap) -> Result<Option<HeaderValue>, ()> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(None);
    };
    let text = origin.to_str().map_err(|_| ())?;
    if text.starts_with("chrome-extension://") || text.starts_with("moz-extension://") {
        Ok(Some(origin.clone()))
    } else {
        Err(())
    }
}

fn with_cors(mut response: Response, origin: Option<HeaderValue>) -> Response {
    if let Some(origin) = origin {
        let h = response.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type"),
        );
    }
    response
}

async fn preflight(headers: HeaderMap) -> Response {
    match check_origin(&headers) {
        Ok(origin) => with_cors(StatusCode::NO_CONTENT.into_response(), origin),
        Err(()) => StatusCode::FORBIDDEN.into_response(),
    }
}

async fn report(
    State(state): State<Arc<BrowserState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let origin = match check_origin(&headers) {
        Ok(origin) => origin,
        Err(()) => return StatusCode::FORBIDDEN.into_response(),
    };
    let Ok(payload) = serde_json::from_slice::<Payload>(&body) else {
        return with_cors(StatusCode::BAD_REQUEST.into_response(), origin);
    };
    state.update(&payload.hostname, payload.has_password_field);
    with_cors(StatusCode::NO_CONTENT.into_response(), origin)
}

pub fn router(state: Arc<BrowserState>) -> Router {
    Router::new()
        .route("/browser-status", post(report).options(preflight))
        .with_state(state)
}

/// Start de bridge op 127.0.0.1; geeft het echte adres terug (nuttig bij poort 0).
pub async fn serve(state: Arc<BrowserState>, port: u16) -> Result<SocketAddr> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = router(state);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(error = %e, "browser-bridge gestopt");
        }
    });
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const FRESH: Duration = Duration::from_secs(60);

    #[test]
    fn melding_wordt_genormaliseerd_en_verloopt() {
        let state = BrowserState::default();
        assert_eq!(state.current(FRESH), None);
        state.update("  MijnBank.com ", true);
        assert_eq!(state.current(FRESH), Some(("mijnbank.com".into(), true)));
        assert_eq!(state.current(Duration::ZERO), None, "verlopen melding telt niet");
    }

    #[test]
    fn lege_hostnaam_wist_de_vorige_site() {
        let state = BrowserState::default();
        state.update("mijnbank.com", true);
        state.update("", false);
        assert_eq!(state.current(FRESH), None);
    }

    fn headers(origin: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = origin {
            h.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn alleen_extensies_en_originloze_clients_mogen() {
        assert_eq!(check_origin(&headers(None)), Ok(None));
        assert!(check_origin(&headers(Some("chrome-extension://abcdef"))).is_ok());
        assert!(check_origin(&headers(Some("moz-extension://abcdef"))).is_ok());
        assert!(check_origin(&headers(Some("https://evil.example"))).is_err());
        assert!(check_origin(&headers(Some("null"))).is_err());
    }

    fn post_raw(addr: SocketAddr, origin: Option<&str>, body: &str) -> String {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        let origin_line = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
        write!(
            stream,
            "POST /browser-status HTTP/1.1\r\nHost: localhost\r\n{origin_line}Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bridge_accepteert_extensie_en_weigert_webpagina() {
        let state = Arc::new(BrowserState::default());
        let addr = serve(Arc::clone(&state), 0).await.unwrap();
        let body = r#"{"hostname":"Bank.nl","hasPasswordField":true}"#;

        let evil = post_raw(addr, Some("https://evil.example"), body);
        assert!(evil.starts_with("HTTP/1.1 403"), "{evil}");
        assert_eq!(state.current(FRESH), None, "webpagina mag de staat niet zetten");

        let ok = post_raw(addr, Some("chrome-extension://abc"), body);
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert!(ok.to_lowercase().contains("access-control-allow-origin: chrome-extension://abc"));
        assert_eq!(state.current(FRESH), Some(("bank.nl".into(), true)));

        let bad = post_raw(addr, None, "geen json");
        assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
    }
}
