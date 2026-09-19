//! Stuurt gefilterde tekst naar een Stash-ingest op je VPS (`deploy/stash-ingest`).
//!
//! Achter de shipper zit de curatiestraat: Stash (ruwe buffer) → selection-pass
//! (LLM-relevantiefilter) → Hindsight. Chronicle levert alleen `index_text`:
//! de tekst zonder terugkerende menubalken en zonder gevoelige patronen, dus
//! de LLM betaalt niet voor chrome.
//!
//! Betrouwbaarheid komt van een cursor in SQLite (`ship_cursor`), niet van
//! bestandjes: de cursor schuift pas op nadat Stash het gehele blok met 2xx
//! bevestigde. Mislukt een post, dan gaat dezelfde batch de volgende ronde
//! opnieuw; Stash dedupliceert op (ts, app, window_title), dus een
//! dubbel-verzonden batch levert geen dubbele rijen op.
//!
//! De eerste keer dat de shipper draait begint hij bij de nieuwste capture, niet
//! bij de hele historie.

use crate::config::ShipConfig;
use crate::embeddings::CaptureRow;
use crate::store::Db;
use anyhow::{Context, Result};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Zo groot is een batch uit `Db::fetch_captures_since`.
const DB_BATCH: usize = 500;

fn to_ndjson(rows: &[CaptureRow]) -> String {
    rows.iter()
        .map(|r| {
            json!({
                "ts": r.ts as f64,
                "app": r.app,
                "window_title": r.title,
                "text": r.index_text,
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Verstuurt alles wat sinds de cursor is bijgekomen. Geeft het aantal
/// verzonden captures terug.
async fn ship_once(
    client: &reqwest::Client,
    cfg: &ShipConfig,
    token: Option<&str>,
    db: &Arc<Db>,
) -> Result<usize> {
    let mut cursor = match db.load_ship_cursor()? {
        Some(cursor) => cursor,
        None => {
            let start = db.max_capture_id()?;
            db.save_ship_cursor(start)?;
            tracing::info!(vanaf_capture = start, "shipper start bij de nieuwste capture");
            start
        }
    };

    let mut shipped = 0;
    loop {
        let rows = tokio::task::spawn_blocking({
            let db = Arc::clone(db);
            move || db.fetch_captures_since(cursor)
        })
        .await
        .context("lezen voor shipper paniekte")??;

        let Some(last) = rows.last() else {
            return Ok(shipped);
        };
        let last_id = last.id;

        let mut request = client
            .post(&cfg.endpoint)
            .header("Content-Type", "application/x-ndjson")
            .body(to_ndjson(&rows));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .await
            .context("Stash onbereikbaar")?
            .error_for_status()
            .context("Stash weigerde de batch")?;

        db.save_ship_cursor(last_id)?;
        cursor = last_id;
        shipped += rows.len();

        if rows.len() < DB_BATCH {
            return Ok(shipped);
        }
    }
}

pub async fn run(cfg: ShipConfig, db: Arc<Db>, mut shutdown: watch::Receiver<bool>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(error = %e, "shipper kon geen HTTP-client bouwen");
            return;
        }
    };
    let token = cfg.resolved_token();
    if token.is_none() {
        tracing::warn!("geen STASH_AUTH_TOKEN of ship.auth_token: Stash accepteert dit alleen zonder INGEST_TOKEN");
    }
    tracing::info!(endpoint = cfg.endpoint, "shipper actief");

    let interval = Duration::from_secs_f64(cfg.interval_secs.max(10.0));
    loop {
        match ship_once(&client, &cfg, token.as_deref(), &db).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(captures = n, "naar Stash verzonden"),
            // Nooit fataal: de opname loopt door en de cursor bewaart de achterstand.
            Err(e) => tracing::warn!(error = format!("{e:#}"), "shippen mislukt; volgende ronde opnieuw"),
        }

        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewCapture;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Received {
        bodies: Arc<Mutex<Vec<String>>>,
        auth: Arc<Mutex<Vec<String>>>,
        fail: Arc<Mutex<bool>>,
    }

    async fn ingest(State(rx): State<Received>, headers: HeaderMap, body: String) -> StatusCode {
        if *rx.fail.lock().unwrap() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        rx.bodies.lock().unwrap().push(body);
        rx.auth.lock().unwrap().push(
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
        );
        StatusCode::OK
    }

    async fn fake_stash() -> (String, Received) {
        let rx = Received::default();
        let app = Router::new().route("/ingest", post(ingest)).with_state(rx.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/ingest"), rx)
    }

    fn seed(db: &Db, ts: i64, app: &str, text: &str) -> i64 {
        let app_id = db.app_id(app, &format!("{app}.exe"), "").unwrap();
        let seg = db.open_segment(app_id, "venster", ts).unwrap();
        db.insert_capture(NewCapture {
            segment_id: seg,
            ts,
            kind: "text",
            source: "uia",
            monitor: "m",
            phash: 1,
            quality: 0.9,
            text,
            index_text: text,
            frame_path: None,
            width: 1,
            height: 1,
            fallback_reason: None,
        })
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn eerste_keer_begint_bij_nu_daarna_alleen_nieuwe_captures() {
        let (endpoint, received) = fake_stash().await;
        let cfg = ShipConfig {
            enabled: true,
            endpoint,
            ..Default::default()
        };
        let db = Arc::new(Db::open_in_memory().unwrap());
        let client = reqwest::Client::new();

        seed(&db, 1_000, "oud", "historie die niet mee mag");
        assert_eq!(ship_once(&client, &cfg, Some("geheim"), &db).await.unwrap(), 0);
        assert!(received.bodies.lock().unwrap().is_empty());

        seed(&db, 2_000, "code", "nieuwe regel tekst");
        assert_eq!(ship_once(&client, &cfg, Some("geheim"), &db).await.unwrap(), 1);
        {
            let bodies = received.bodies.lock().unwrap();
            assert_eq!(bodies.len(), 1);
            let line: serde_json::Value = serde_json::from_str(bodies[0].trim()).unwrap();
            assert_eq!(line["app"], "code");
            assert_eq!(line["window_title"], "venster");
            assert_eq!(line["text"], "nieuwe regel tekst");
            assert_eq!(line["ts"], 2000.0);
            assert_eq!(received.auth.lock().unwrap()[0], "Bearer geheim");
        }

        // Niets nieuws: niets versturen.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 0);
        assert_eq!(received.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mislukte_post_laat_de_cursor_staan_en_probeert_opnieuw() {
        let (endpoint, received) = fake_stash().await;
        let cfg = ShipConfig {
            enabled: true,
            endpoint,
            ..Default::default()
        };
        let db = Arc::new(Db::open_in_memory().unwrap());
        let client = reqwest::Client::new();

        // Initialiseer de cursor voordat er nieuwe captures zijn.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 0);
        seed(&db, 3_000, "code", "moet blijven liggen tot Stash terug is");

        *received.fail.lock().unwrap() = true;
        assert!(ship_once(&client, &cfg, None, &db).await.is_err());
        assert!(received.bodies.lock().unwrap().is_empty());

        *received.fail.lock().unwrap() = false;
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 1);
        assert_eq!(received.bodies.lock().unwrap().len(), 1);
    }
}
