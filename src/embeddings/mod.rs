pub mod document;
pub mod provider;
pub mod store;
pub mod indexer;

pub use document::{SemanticDocument, CaptureRow, build_documents, content_hash};
pub use provider::{EmbeddingProvider, FastEmbedProvider, MockProvider};
pub use store::{VectorStore, InMemoryStore, PgVectorStore, SemanticHit};
pub use indexer::{Indexer, IndexCommand};

use std::sync::Arc;

use crate::config::Config;
use crate::store::Db;

/// Helper om de juiste store te maken op basis van config.
pub fn make_store(cfg: &Config) -> Arc<dyn VectorStore> {
    if let Some(url) = cfg.embeddings_postgres_url() {
        let store = PgVectorStore::new(url, cfg.embeddings.dimensions, cfg.embeddings.model.clone());
        Arc::new(store) as Arc<dyn VectorStore>
    } else {
        Arc::new(InMemoryStore::new()) as Arc<dyn VectorStore>
    }
}

/// Helper om provider te maken.
///
/// `async` omdat `fastembed`-provider laden een blokkerende operatie is (bij
/// een lege cache: een download van ~118 MB) — dat hoort nooit rechtstreeks
/// op de tokio-runtime te draaien. Faalt het laden (geen internet bij de
/// eerste run, ONNX-runtime probleem), dan valt dit terug op `MockProvider`
/// met een duidelijke waarschuwing in plaats van capture te blokkeren of
/// `capture start` te laten crashen op een optionele feature.
pub async fn make_provider(cfg: &Config) -> Arc<dyn EmbeddingProvider> {
    if cfg.embeddings.provider == "fastembed" {
        let cache_dir = cfg.models_dir();
        let result = match cache_dir {
            Ok(dir) => tokio::task::spawn_blocking(move || FastEmbedProvider::new(dir)).await,
            Err(e) => return fallback_mock(cfg, &e.to_string()),
        };
        match result {
            Ok(Ok(provider)) => return Arc::new(provider) as Arc<dyn EmbeddingProvider>,
            Ok(Err(e)) => return fallback_mock(cfg, &e.to_string()),
            Err(e) => return fallback_mock(cfg, &format!("laad-taak panicked: {e}")),
        }
    }
    Arc::new(MockProvider::new(
        cfg.embeddings.dimensions,
        cfg.embeddings.model.clone(),
    )) as Arc<dyn EmbeddingProvider>
}

fn fallback_mock(cfg: &Config, reason: &str) -> Arc<dyn EmbeddingProvider> {
    tracing::warn!(
        error = reason,
        "fastembed-provider laden mislukt — val terug op mock (semantic search vindt \
         dan alleen letterlijke herhalingen, geen verwante tekst)"
    );
    Arc::new(MockProvider::new(
        cfg.embeddings.dimensions,
        cfg.embeddings.model.clone(),
    )) as Arc<dyn EmbeddingProvider>
}

/// Factory voor indexer — gebruikt in pipeline en serve.
pub async fn make_indexer(
    cfg: Config,
    db: Arc<Db>,
) -> (Indexer, Option<tokio::sync::mpsc::UnboundedSender<IndexCommand>>) {
    let store = make_store(&cfg);
    let provider = make_provider(&cfg).await;
    // Channel voor directe capture notificaties (optioneel, poll blijft werken).
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let indexer = Indexer::new(cfg, db, store, provider, Some(rx));
    (indexer, Some(tx))
}
