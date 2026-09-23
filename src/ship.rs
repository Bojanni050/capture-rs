//! Stuurt gefilterde tekst naar de Foundation Ingestie Gateway op je VPS
//! (POST /api/ingest/capture, zie docs/foundation-gateway.md).
//!
//! Elke capture met tekst gaat als eigen JSON-object naar de Gateway; de
//! Gateway bepaalt status ("observation") en slaat hem op in Postgres. De
//! identiteit van een capture ligt in url = capture://capture/<id>, dus
//! een dubbel-verzonden capture wordt door Foundation ge-updated in plaats
//! van verdubbeld.
//!
//! Betrouwbaarheid komt van een cursor in SQLite (ship_cursor), niet van
//! bestandjes: de cursor schuift pas op nadat de Gateway de hele batch met
//! 2xx bevestigde. Mislukt een post, dan gaat dezelfde batch de volgende
//! ronde opnieuw; door de url-dedup levert dat geen dubbele rijen op.
//!
//! Captures zonder tekst (alleen beeld) worden overgeslagen; de cursor
//! schuift wel over ze heen zodat ze niet blijven blokkeren.
//!
//! De eerste keer dat de shipper draait begint hij bij de nieuwste capture,
//! niet bij de hele historie.

use crate::config::ShipConfig;
use crate::embeddings::CaptureRow;
use crate::store::Db;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Zo groot is een batch uit Db::fetch_captures_since.
const DB_BATCH: usize = 500;

fn rfc3339(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is geldig"))
        .to_rfc3339()
}

/// JSON-payload voor een capture voor de Foundation Gateway.
fn to_payload(row: &CaptureRow) -> serde_json::Value {
    json!({
        "content": row.index_text,
        "source": "capture-rs",
        "title": format!("{} — {}", row.app, row.title),
        "url": format!("capture://capture/{}", row.id),
        "tags": [row.app],
        "occurredAt": rfc3339(row.ts),
    })
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

        // Per capture een eigen POST; mislukt er een, dan blijft de cursor
        // staan en gaat de hele batch de volgende ronde opnieuw (idempotent
        // via de url).
        for row in rows.iter().filter(|r| !r.index_text.trim().is_empty()) {
            let mut request = client
                .post(&cfg.endpoint)
                .header("Content-Type", "application/json")
                .body(to_payload(row).to_string());
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            request
                .send()
                .await
                .context("Foundation Gateway onbereikbaar")?
                .error_for_status()
                .context("Foundation Gateway weigerde de capture")?;
            shipped += 1;
        }

        db.save_ship_cursor(last_id)?;
        cursor = last_id;

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
        tracing::warn!("geen CAPTURE_INGEST_TOKEN of ship.auth_token: Foundation Gateway weigert zonder Bearer-token");
    }
    tracing::info!(endpoint = cfg.endpoint, "shipper actief");

    let interval = Duration::from_secs_f64(cfg.interval_secs.max(10.0));
    loop {
        match ship_once(&client, &cfg, token.as_deref(), &db).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(captures = n, "naar Foundation Gateway verzonden"),
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
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        auth: Arc<Mutex<Vec<String>>>,
        fail: Arc<Mutex<bool>>,
    }

    async fn ingest(State(rx): State<Received>, headers: HeaderMap, body: String) -> StatusCode {
        if *rx.fail.lock().unwrap() {
            return StatusCode::UNPROCESSABLE_ENTITY;
        }
        rx.bodies.lock().unwrap().push(serde_json::from_str(&body).expect("geldig JSON"));
        rx.auth.lock().unwrap().push(
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
        );
        StatusCode::OK
    }

    async fn fake_gateway() -> (String, Received) {
        let rx = Received::default();
        let app = Router::new().route("/api/ingest/capture", post(ingest)).with_state(rx.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/api/ingest/capture"), rx)
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
        let (endpoint, received) = fake_gateway().await;
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
            assert_eq!(bodies[0]["content"], "nieuwe regel tekst");
            assert_eq!(bodies[0]["source"], "capture-rs");
            assert_eq!(bodies[0]["title"], "code — venster");
            assert_eq!(bodies[0]["tags"][0], "code");
            assert!(bodies[0]["url"].as_str().unwrap().starts_with("capture://capture/"));
            assert!(bodies[0]["occurredAt"].as_str().unwrap().starts_with("1970-01-01"));
            assert_eq!(received.auth.lock().unwrap()[0], "Bearer geheim");
        }

        // Niets nieuws: niets versturen.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 0);
        assert_eq!(received.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mislukte_post_laat_de_cursor_staan_en_probeert_opnieuw() {
        let (endpoint, received) = fake_gateway().await;
        let cfg = ShipConfig {
            enabled: true,
            endpoint,
            ..Default::default()
        };
        let db = Arc::new(Db::open_in_memory().unwrap());
        let client = reqwest::Client::new();

        // Initialiseer de cursor voordat er nieuwe captures zijn.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 0);
        seed(&db, 3_000, "code", "moet blijven liggen tot de Gateway terug is");

        *received.fail.lock().unwrap() = true;
        assert!(ship_once(&client, &cfg, None, &db).await.is_err());
        assert!(received.bodies.lock().unwrap().is_empty());

        *received.fail.lock().unwrap() = false;
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 1);
        assert_eq!(received.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lege_tekst_wordt_overgeslagen_maar_schuift_de_cursor_door() {
        let (endpoint, received) = fake_gateway().await;
        let cfg = ShipConfig {
            enabled: true,
            endpoint,
            ..Default::default()
        };
        let db = Arc::new(Db::open_in_memory().unwrap());
        let client = reqwest::Client::new();

        // Cursor initialiseren.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 0);
        seed(&db, 4_000, "beeld", "   ");
        let tekst_id = seed(&db, 4_100, "code", "wel tekst");
        seed(&db, 4_200, "beeld", "");

        // Alleen de capture met tekst gaat erheen, maar de cursor schuift
        // door tot de laatste (lege) capture.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 1);
        {
            let bodies = received.bodies.lock().unwrap();
            assert_eq!(bodies.len(), 1);
            assert_eq!(bodies[0]["content"], "wel tekst");
            assert_eq!(bodies[0]["url"], format!("capture://capture/{tekst_id}"));
        }

        // Tweede ronde: niets meer te doen, ook de lege niet opnieuw.
        assert_eq!(ship_once(&client, &cfg, None, &db).await.unwrap(), 0);
        assert_eq!(received.bodies.lock().unwrap().len(), 1);
    }
}
