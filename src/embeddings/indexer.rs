//! Asynchrone indexer — bouwt semantic documents uit SQLite en schrijft naar vector store.

use std::sync::Arc;

use anyhow::Result;

use crate::config::Config;
use crate::embeddings::document::{CaptureRow, build_documents};
use crate::embeddings::provider::EmbeddingProvider;
use crate::embeddings::store::VectorStore;
use crate::store::Db;

/// Indexer die periodiek SQLite polled en embeddings bijwerkt.
pub struct Indexer {
    cfg: Config,
    db: Arc<Db>,
    store: Arc<dyn VectorStore>,
    provider: Arc<dyn EmbeddingProvider>,
}

impl Indexer {
    pub fn new(
        cfg: Config,
        db: Arc<Db>,
        store: Arc<dyn VectorStore>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            cfg,
            db,
            store,
            provider,
        }
    }

    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        if !self.cfg.embeddings.enabled {
            tracing::info!("embeddings uitgeschakeld — indexer slaapt");
            // Wacht tot shutdown, doe niets.
            while !*shutdown.borrow() {
                let _ = shutdown.changed().await;
            }
            return Ok(());
        }

        // Ensure schema — als PG down of geen vector extensie, log en retry later.
        if let Err(e) = self.store.ensure_schema().await {
            tracing::warn!(error = %e, "vector store schema niet beschikbaar — semantic indexing pending");
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_secs_f64(
            self.cfg.embeddings.poll_interval_secs.max(1.0),
        ));
        // Sla eerste tick over
        interval.tick().await;

        let mut last_seen_id = self.initial_cursor().await?;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.tick_once(&mut last_seen_id).await {
                        Ok(n) if n > 0 => tracing::info!(ingedeeld = n, "semantic documents geïndexeerd"),
                        Ok(_) => {},
                        Err(e) => tracing::warn!(error = %e, "indexer tick faalde — retry later"),
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
            }
        }
        Ok(())
    }

    async fn tick_once(&self, last_seen: &mut i64) -> Result<usize> {
        // Lees nieuwe captures uit SQLite (blocking → spawn_blocking).
        let db = Arc::clone(&self.db);
        let from = *last_seen;
        let window = self.cfg.embeddings.window_secs;
        let max_chars = self.cfg.embeddings.max_content_chars;
        let min_chars = self.cfg.embeddings.min_chars;

        // We lezen in batches via spawn_blocking om tokio niet te blokkeren.
        let rows: Vec<CaptureRow> = tokio::task::spawn_blocking(move || fetch_since(&db, from))
            .await
            .map_err(|e| anyhow::anyhow!("fetch task panicked: {e}"))??;

        if rows.is_empty() {
            return Ok(0);
        }
        // Waar deze batch tot komt — pas vastgelegd zodra we er ook echt iets
        // mee gedaan hebben. Verder dan dit gaan we vandaag niet terug, ook
        // al levert de batch zelf geen documenten op (te kort/gefilterd):
        // die rijen leveren bij een volgende poging evengoed niets nieuws op.
        let advanced_to = rows.iter().map(|r| r.id).max().unwrap_or(*last_seen);

        // Groepeer tot documents deterministisch.
        let docs = build_documents(
            &rows,
            window,
            max_chars,
            min_chars,
            self.provider.model_name(),
            self.provider.dimensions(),
        );
        if docs.is_empty() {
            self.commit_cursor(last_seen, advanced_to).await?;
            return Ok(0);
        }

        // Embed batch
        let texts: Vec<String> = docs.iter().map(|d| d.content.clone()).collect();
        let provider = Arc::clone(&self.provider);
        let embeddings = tokio::task::spawn_blocking(move || provider.embed(&texts))
            .await
            .map_err(|e| anyhow::anyhow!("embed task panicked: {e}"))??;

        // Upsert — als PG down, geeft dit een fout en stopt tick_once hier via
        // `?`. De cursor wordt dan bewust NIET bijgewerkt: deze batch blijft
        // "nieuw" voor de volgende tik, zodat een teruggekomen PG hem alsnog
        // oppakt in plaats van dat hij stilzwijgend overgeslagen wordt.
        let n = self.store.upsert(docs, embeddings).await?;
        self.commit_cursor(last_seen, advanced_to).await?;
        Ok(n)
    }

    async fn initial_cursor(&self) -> Result<i64> {
        if !self.store.is_durable() {
            // In-memory documenten verdwijnen bij afsluiten; de proceslokale
            // index moet daarom altijd opnieuw vanaf de oudste capture beginnen.
            return Ok(0);
        }

        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.load_embeddings_cursor())
            .await
            .map_err(|e| anyhow::anyhow!("cursor lezen panicked: {e}"))?
    }

    /// Werkt de cursor bij. Alleen een duurzame vector store mag de voortgang
    /// in SQLite bewaren; vluchtige stores beginnen na een restart opnieuw.
    async fn commit_cursor(&self, last_seen: &mut i64, advanced_to: i64) -> Result<()> {
        if self.store.is_durable() {
            let db = Arc::clone(&self.db);
            tokio::task::spawn_blocking(move || db.save_embeddings_cursor(advanced_to))
                .await
                .map_err(|e| anyhow::anyhow!("cursor opslaan panicked: {e}"))??;
        }
        *last_seen = advanced_to;
        Ok(())
    }
}

// Helper: lees captures sinds id, alleen `text` soort en met index_text niet leeg.
fn fetch_since(db: &Db, since_id: i64) -> Result<Vec<CaptureRow>> {
    db.fetch_captures_since(since_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::provider::MockProvider;
    use crate::embeddings::store::InMemoryStore;
    use crate::store::NewCapture;

    #[tokio::test]
    async fn in_memory_indexer_herindexeert_na_restart_en_bewaart_geen_cursor() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.save_embeddings_cursor(99).unwrap();
        let app_id = db.app_id("editor", "editor.exe", "").unwrap();
        let segment_id = db.open_segment(app_id, "project", 1_700_000_000).unwrap();
        let text = "Dit is een voldoende lange tekstregel om een semantisch document te maken.";
        db.insert_capture(NewCapture {
            segment_id,
            ts: 1_700_000_000,
            kind: "text",
            source: "uia",
            monitor: "primary",
            phash: 0,
            quality: 1.0,
            text,
            index_text: text,
            frame_path: None,
            width: 100,
            height: 100,
            fallback_reason: None,
        })
        .unwrap();

        let store = Arc::new(InMemoryStore::new());
        let indexer = Indexer::new(
            Config::default(),
            Arc::clone(&db),
            store.clone(),
            Arc::new(MockProvider::default()),
        );

        let mut cursor = indexer.initial_cursor().await.unwrap();
        assert_eq!(cursor, 0, "volatile store mag oude cursor niet vertrouwen");
        indexer.tick_once(&mut cursor).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);
        assert_eq!(cursor, 1);
        assert_eq!(db.load_embeddings_cursor().unwrap(), 99);

        // Een nieuwe indexer met een lege RAM-store moet dezelfde historie
        // opnieuw ophalen, ook al staat SQLite nog op een hogere cursor.
        let restarted = Indexer::new(
            Config::default(),
            db,
            Arc::new(InMemoryStore::new()),
            Arc::new(MockProvider::default()),
        );
        assert_eq!(restarted.initial_cursor().await.unwrap(), 0);
    }
}
