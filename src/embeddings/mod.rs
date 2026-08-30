pub mod document;
pub mod provider;
pub mod store;
pub mod indexer;

pub use document::{SemanticDocument, CaptureRow, build_documents, content_hash};
pub use provider::{EmbeddingProvider, MockProvider};
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
pub fn make_provider(cfg: &Config) -> Arc<dyn EmbeddingProvider> {
    // Voor v1 alleen mock; fastembed achter feature later.
    Arc::new(MockProvider::new(
        cfg.embeddings.dimensions,
        cfg.embeddings.model.clone(),
    )) as Arc<dyn EmbeddingProvider>
}

/// Factory voor indexer — gebruikt in pipeline en serve.
pub fn make_indexer(
    cfg: Config,
    db: Arc<Db>,
) -> (Indexer, Option<tokio::sync::mpsc::UnboundedSender<IndexCommand>>) {
    let store = make_store(&cfg);
    let provider = make_provider(&cfg);
    // Channel voor directe capture notificaties (optioneel, poll blijft werken).
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let indexer = Indexer::new(cfg, db, store, provider, Some(rx));
    (indexer, Some(tx))
}
