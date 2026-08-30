//! Embedding provider abstraction — lokaal, vervangbaar.

use anyhow::Result;

/// Trait voor lokale embedding modellen.
///
/// Implementaties moeten deterministisch zijn per `model_name`+`dimensions`.
/// Batch-variant is primair (voordeliger voor ONNX).
pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
    fn model_name(&self) -> &str;
}

/// Deterministische mock voor tests en wanneer geen model geïnstalleerd is.
///
/// Genereert een pseudo-embedding via FNV-hash van de tekst, genormaliseerd.
/// Niet semantisch, wel deterministisch, goedkoop en reproduceerbaar.
pub struct MockProvider {
    dims: usize,
    model: String,
}

impl MockProvider {
    pub fn new(dimensions: usize, model: impl Into<String>) -> Self {
        Self {
            dims: dimensions.max(1).min(4096),
            model: model.into(),
        }
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new(384, "multilingual-e5-small-mock")
    }
}

impl EmbeddingProvider for MockProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| mock_embed(t, self.dims)).collect())
    }
    fn dimensions(&self) -> usize {
        self.dims
    }
    fn model_name(&self) -> &str {
        &self.model
    }
}

fn mock_embed(text: &str, dims: usize) -> Vec<f32> {
    // FNV-1a per token-achtige sharding, dan L2 normalisatie.
    let mut out = vec![0f32; dims];
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return out;
    }
    // Hash per dimensie met verschillende seeds voor spreiding.
    for i in 0..dims {
        let mut h: u64 = 0xcbf29ce484222325u64 ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15);
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        // Map hash naar [-1,1]
        let v = ((h as f64 / u64::MAX as f64) * 2.0 - 1.0) as f32;
        out[i] = v;
    }
    // L2 normaliseren (cosine search verwacht genormaliseerd)
    let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-6 {
        for v in &mut out {
            *v /= norm;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_is_deterministic() {
        let p = MockProvider::new(8, "mock");
        let a = p.embed(&["hello world".into()]).unwrap();
        let b = p.embed(&["hello world".into()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn mock_different_texts_differ() {
        let p = MockProvider::new(8, "mock");
        let a = p.embed(&["hello".into()]).unwrap()[0].clone();
        let b = p.embed(&["goodbye".into()]).unwrap()[0].clone();
        assert_ne!(a, b);
    }

    #[test]
    fn mock_is_normalized() {
        let p = MockProvider::new(16, "mock");
        let v = p.embed(&["test".into()]).unwrap().into_iter().next().unwrap();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }
}
