//! Vector store abstraction — pgvector primair, in-memory fallback voor tests.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::document::SemanticDocument;

/// Resultaat van een semantische zoekopdracht met provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticHit {
    pub document: SemanticDocument,
    pub score: f32, // cosine similarity 0..1 (hoger = meer vergelijkbaar)
    pub snippet: String, // eerste 220 chars van content
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn ensure_schema(&self) -> Result<()>;
    async fn upsert(&self, docs: Vec<SemanticDocument>, embeddings: Vec<Vec<f32>>) -> Result<usize>;
    async fn search(&self, query_embedding: Vec<f32>, limit: usize) -> Result<Vec<SemanticHit>>;
    async fn delete_before(&self, cutoff_ts: i64) -> Result<usize>;
    async fn count(&self) -> Result<usize>;
    fn is_available(&self) -> bool;
}

// ---------------------------------------------------------------------------
// In-memory fallback — geen PG nodig, gebruikt voor tests en når PG down
// is.
// ---------------------------------------------------------------------------

pub struct InMemoryStore {
    docs: std::sync::RwLock<Vec<(SemanticDocument, Vec<f32>)>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            docs: std::sync::RwLock::new(Vec::new()),
        }
    }
}

#[async_trait]
impl VectorStore for InMemoryStore {
    async fn ensure_schema(&self) -> Result<()> {
        Ok(())
    }
    async fn upsert(&self, docs: Vec<SemanticDocument>, embeddings: Vec<Vec<f32>>) -> Result<usize> {
        let mut guard = self.docs.write().unwrap();
        let mut n = 0;
        for (doc, emb) in docs.into_iter().zip(embeddings) {
            // Dedup op content_hash
            if guard.iter().any(|(d, _)| d.content_hash == doc.content_hash) {
                continue;
            }
            guard.push((doc, emb));
            n += 1;
        }
        Ok(n)
    }
    async fn search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<SemanticHit>> {
        let guard = self.docs.read().unwrap();
        let mut scored: Vec<_> = guard
            .iter()
            .map(|(doc, emb)| {
                let score = cosine(&query, emb);
                (
                    SemanticHit {
                        document: doc.clone(),
                        score,
                        snippet: doc.content.chars().take(220).collect(),
                    },
                    score,
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        Ok(scored.into_iter().take(limit).map(|(h, _)| h).collect())
    }
    async fn delete_before(&self, cutoff: i64) -> Result<usize> {
        let mut guard = self.docs.write().unwrap();
        let before = guard.len();
        guard.retain(|(d, _)| d.end_time >= cutoff);
        Ok(before - guard.len())
    }
    async fn count(&self) -> Result<usize> {
        Ok(self.docs.read().unwrap().len())
    }
    fn is_available(&self) -> bool {
        true
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < 1e-6 || nb < 1e-6 {
        0.0
    } else {
        (dot / (na * nb)).clamp(-1.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// PostgreSQL + pgvector — echte opslag
// ---------------------------------------------------------------------------

pub struct PgVectorStore {
    url: String,
    dimensions: usize,
    model: String,
}

impl PgVectorStore {
    pub fn new(url: impl Into<String>, dimensions: usize, model: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            dimensions,
            model: model.into(),
        }
    }

    fn vector_literal(emb: &[f32]) -> String {
        // pgvector text format: '[0.1,0.2,...]'
        let mut s = String::from("[");
        for (i, v) in emb.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("{v:.6}"));
        }
        s.push(']');
        s
    }
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn ensure_schema(&self) -> Result<()> {
        let (client, conn) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
            .await
            .context("postgres connect faalde")?;
        tokio::spawn(async move { let _ = conn.await; });

        // Probeer vector extensie — als niet beschikbaar, geef duidelijke fout.
        let ext = client
            .query_opt(
                "SELECT 1 FROM pg_available_extensions WHERE name='vector'",
                &[],
            )
            .await?;
        if ext.is_none() {
            anyhow::bail!("pgvector extensie 'vector' niet beschikbaar — installeer pgvector voor PG 18 (zie docs/embeddings-proposal.md)");
        }
        client
            .execute("CREATE EXTENSION IF NOT EXISTS vector", &[])
            .await
            .context("CREATE EXTENSION vector faalde")?;

        let dims = self.dimensions as i32;
        // Gebruik string interpolatie voor dims (geen param voor type modifier)
        let ddl = format!(
            r#"
            CREATE TABLE IF NOT EXISTS semantic_documents (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL UNIQUE,
                application TEXT NOT NULL,
                title TEXT NOT NULL,
                start_time BIGINT NOT NULL,
                end_time BIGINT NOT NULL,
                source_capture_ids BIGINT[] NOT NULL,
                source TEXT NOT NULL,
                has_frame BOOL NOT NULL,
                embedding VECTOR({dims}) NOT NULL,
                embedding_model TEXT NOT NULL,
                embedding_dimensions INT NOT NULL,
                created_at BIGINT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_semantic_app_time ON semantic_documents(application, start_time);
            CREATE INDEX IF NOT EXISTS idx_semantic_hash ON semantic_documents(content_hash);
            "#
        );
        client.batch_execute(&ddl).await?;

        // IVFFlat index pas na data; probeer aan te maken, negeer als leeg.
        let _ = client
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_semantic_embedding ON semantic_documents USING ivfflat (embedding vector_cosine_ops) WITH (lists=100)",
                &[],
            )
            .await;

        Ok(())
    }

    async fn upsert(&self, docs: Vec<SemanticDocument>, embeddings: Vec<Vec<f32>>) -> Result<usize> {
        if docs.is_empty() {
            return Ok(0);
        }
        let (client, conn) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
            .await
            .context("postgres connect faalde")?;
        tokio::spawn(async move { let _ = conn.await; });

        let mut n = 0;
        for (doc, emb) in docs.into_iter().zip(embeddings) {
            let vec_lit = Self::vector_literal(&emb);
            // Upsert op content_hash
            let res = client
                .execute(
                    "INSERT INTO semantic_documents (id, content, content_hash, application, title, start_time, end_time, source_capture_ids, source, has_frame, embedding, embedding_model, embedding_dimensions, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::vector, $12, $13, $14)
                     ON CONFLICT (content_hash) DO NOTHING",
                    &[
                        &doc.id,
                        &doc.content,
                        &doc.content_hash,
                        &doc.application,
                        &doc.title,
                        &doc.start_time,
                        &doc.end_time,
                        &doc.source_capture_ids,
                        &doc.source,
                        &doc.has_frame,
                        &vec_lit,
                        &doc.embedding_model,
                        &(doc.embedding_dimensions as i32),
                        &doc.created_at,
                    ],
                )
                .await?;
            if res > 0 {
                n += 1;
            }
        }
        Ok(n)
    }

    async fn search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<SemanticHit>> {
        let (client, conn) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
            .await
            .context("postgres connect faalde")?;
        tokio::spawn(async move { let _ = conn.await; });

        let vec_lit = Self::vector_literal(&query);
        let lim = limit as i64;
        let rows = client
            .query(
                "SELECT id, content, content_hash, application, title, start_time, end_time, source_capture_ids, source, has_frame, embedding_model, embedding_dimensions, created_at,
                        1 - (embedding <=> $1::vector) AS score
                 FROM semantic_documents
                 ORDER BY embedding <=> $1::vector
                 LIMIT $2",
                &[&vec_lit, &lim],
            )
            .await?;

        let mut hits = Vec::new();
        for r in rows {
            let doc = SemanticDocument {
                id: r.get::<_, String>(0),
                content: r.get(1),
                content_hash: r.get(2),
                application: r.get(3),
                title: r.get(4),
                start_time: r.get(5),
                end_time: r.get(6),
                source_capture_ids: r.get(7),
                source: r.get(8),
                has_frame: r.get(9),
                embedding_model: r.get(10),
                embedding_dimensions: r.get::<_, i32>(11) as usize,
                created_at: r.get(12),
            };
            let score: f32 = r.get(13);
            hits.push(SemanticHit {
                snippet: doc.content.chars().take(220).collect(),
                document: doc,
                score,
            });
        }
        Ok(hits)
    }

    async fn delete_before(&self, cutoff: i64) -> Result<usize> {
        let (client, conn) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
            .await
            .context("postgres connect faalde")?;
        tokio::spawn(async move { let _ = conn.await; });
        let n = client
            .execute("DELETE FROM semantic_documents WHERE end_time < $1", &[&cutoff])
            .await?;
        Ok(n as usize)
    }

    async fn count(&self) -> Result<usize> {
        let (client, conn) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
            .await
            .context("postgres connect faalde")?;
        tokio::spawn(async move { let _ = conn.await; });
        let row = client
            .query_one("SELECT COUNT(*) FROM semantic_documents", &[])
            .await?;
        Ok(row.get::<_, i64>(0) as usize)
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::{document::SemanticDocument, provider::MockProvider};
    use crate::embeddings::provider::EmbeddingProvider;

    #[tokio::test]
    async fn in_memory_upsert_en_search() {
        let store = InMemoryStore::new();
        let provider = MockProvider::new(8, "mock");
        let doc = SemanticDocument {
            id: uuid::Uuid::new_v4().to_string(),
            content: "debugging gaia proxy".into(),
            content_hash: "abc".into(),
            application: "code".into(),
            title: "main.rs".into(),
            start_time: 1000,
            end_time: 1001,
            source_capture_ids: vec![1],
            source: "uia".into(),
            has_frame: false,
            embedding_model: "mock".into(),
            embedding_dimensions: 8,
            created_at: 1000,
        };
        let emb = provider.embed(&[doc.content.clone()]).unwrap();
        store.upsert(vec![doc.clone()], emb.clone()).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);

        // Zelfde tekst → cosine ≈ 1.0 met mock (deterministisch genormaliseerd)
        let q_emb = provider.embed(&[doc.content.clone()]).unwrap()[0].clone();
        let hits = store.search(q_emb, 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.99);
    }

    #[tokio::test]
    async fn purge_verwijdert_voor_cutoff() {
        let store = InMemoryStore::new();
        let doc = SemanticDocument {
            id: uuid::Uuid::new_v4().to_string(),
            content: "old content here please".into(),
            content_hash: "old".into(),
            application: "code".into(),
            title: "t".into(),
            start_time: 100,
            end_time: 100,
            source_capture_ids: vec![1],
            source: "uia".into(),
            has_frame: false,
            embedding_model: "mock".into(),
            embedding_dimensions: 384,
            created_at: 100,
        };
        store.upsert(vec![doc], vec![vec![0.1; 384]]).await.unwrap();
        assert_eq!(store.delete_before(200).await.unwrap(), 1);
        assert_eq!(store.count().await.unwrap(), 0);
    }
}
