//! Asynchrone indexer — bouwt semantic documents uit SQLite en schrijft naar vector store.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::embeddings::document::{CaptureRow, SemanticDocument, build_documents};
use crate::embeddings::provider::EmbeddingProvider;
use crate::embeddings::store::VectorStore;
use crate::store::Db;

/// Events die de pipeline kan sturen; voor v1 gebruiken we poll, maar channel is al klaar.
pub enum IndexCommand {
    Capture(i64),
}

/// Indexer die periodiek SQLite polled en embeddings bijwerkt.
pub struct Indexer {
    cfg: Config,
    db: Arc<Db>,
    store: Arc<dyn VectorStore>,
    provider: Arc<dyn EmbeddingProvider>,
    rx: Option<mpsc::UnboundedReceiver<IndexCommand>>,
}

impl Indexer {
    pub fn new(
        cfg: Config,
        db: Arc<Db>,
        store: Arc<dyn VectorStore>,
        provider: Arc<dyn EmbeddingProvider>,
        rx: Option<mpsc::UnboundedReceiver<IndexCommand>>,
    ) -> Self {
        Self {
            cfg,
            db,
            store,
            provider,
            rx,
        }
    }

    pub async fn run(mut self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
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

        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs_f64(self.cfg.embeddings.poll_interval_secs.max(1.0)));
        // Sla eerste tick over
        interval.tick().await;

        // Duurzaam vanaf hier verder, niet vanaf 0: zonder dit herembedde elke
        // herstart de hele geschiedenis in batches van 500 rijen.
        let db = Arc::clone(&self.db);
        let mut last_seen_id: i64 = tokio::task::spawn_blocking(move || db.load_embeddings_cursor())
            .await
            .map_err(|e| anyhow::anyhow!("cursor lezen panicked: {e}"))??;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.tick_once(&mut last_seen_id).await {
                        Ok(n) if n > 0 => tracing::info!(ingedeeld = n, "semantic documents geïndexeerd"),
                        Ok(_) => {},
                        Err(e) => tracing::warn!(error = %e, "indexer tick faalde — retry later"),
                    }
                }
                cmd = async { match &mut self.rx { Some(rx) => rx.recv().await, None => std::future::pending().await } } => {
                    if let Some(IndexCommand::Capture(id)) = cmd {
                        if id > last_seen_id { last_seen_id = id - 1; }
                    } else {
                        // Channel gesloten → alleen poll
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

    /// Werkt de cursor bij in het geheugen én in SQLite, in die volgorde: pas
    /// als de opslag lukt telt de voortgang als definitief.
    async fn commit_cursor(&self, last_seen: &mut i64, advanced_to: i64) -> Result<()> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.save_embeddings_cursor(advanced_to))
            .await
            .map_err(|e| anyhow::anyhow!("cursor opslaan panicked: {e}"))??;
        *last_seen = advanced_to;
        Ok(())
    }
}

// Helper: lees captures sinds id, alleen `text` soort en met index_text niet leeg.
fn fetch_since(db: &Db, since_id: i64) -> Result<Vec<CaptureRow>> {
    db.fetch_captures_since(since_id)
}
