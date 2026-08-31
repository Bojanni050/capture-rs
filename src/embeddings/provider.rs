//! Embedding provider abstraction — lokaal, vervangbaar.

use anyhow::Result;

/// Trait voor lokale embedding modellen.
///
/// Implementaties moeten deterministisch zijn per `model_name`+`dimensions`.
/// Batch-variant is primair (voordeliger voor ONNX).
pub trait EmbeddingProvider: Send + Sync {
    /// Embedt tekst die wordt OPGESLAGEN (een document/passage).
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embedt een ZOEKVRAAG. Asymmetrisch getrainde modellen — E5 leert een
    /// apart `"query: "`-voorvoegsel naast `"passage: "` voor opgeslagen
    /// tekst — overschrijven dit. Voor symmetrische providers (waaronder de
    /// mock) is een zoekvraag gewoon nog een stuk tekst om te embedden.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed(std::slice::from_ref(&text.to_string()))?;
        Ok(out.pop().unwrap_or_default())
    }

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

/// Echt lokaal embeddingmodel: `intfloat/multilingual-e5-small` via
/// `fastembed` (ONNX Runtime, CPU). 384 dimensies, NL+EN.
///
/// `TextEmbedding::embed` vraagt `&mut self`; deze trait vraagt overal
/// `&self`, omdat providers als `Arc<dyn EmbeddingProvider>` gedeeld worden
/// tussen de indexer, de webserver en losse CLI-aanroepen. Eén `Mutex` rond
/// het model lost dat op — embedden is toch al een zware, niet-parallelle
/// stap per aanroep; de aanroepers wikkelen elke aanroep al in
/// `spawn_blocking` zodat dit nooit de tokio-runtime blokkeert.
pub struct FastEmbedProvider {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
    dims: usize,
    model_name: String,
}

impl FastEmbedProvider {
    /// Blokkerend: laadt het model, en downloadt het bij een lege cache
    /// (~118 MB, eenmalig). Roep dit via `spawn_blocking` aan — dit hoort
    /// nooit rechtstreeks op de tokio-runtime te draaien.
    pub fn new(cache_dir: std::path::PathBuf) -> Result<Self> {
        use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

        let info = TextEmbedding::get_model_info(&EmbeddingModel::MultilingualE5Small)
            .map_err(|e| anyhow::anyhow!("modelinfo opvragen mislukt: {e}"))?;
        let dims = info.dim;
        let model_name = info.model_code.clone();

        let options = TextInitOptions::new(EmbeddingModel::MultilingualE5Small)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let model = TextEmbedding::try_new(options)
            .map_err(|e| anyhow::anyhow!("{model_name} laden/downloaden mislukt: {e}"))?;

        Ok(Self {
            model: std::sync::Mutex::new(model),
            dims,
            model_name,
        })
    }
}

impl EmbeddingProvider for FastEmbedProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // E5 is asymmetrisch getraind: opgeslagen tekst krijgt "passage: ".
        let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        model
            .embed(prefixed, None)
            .map_err(|e| anyhow::anyhow!("fastembed embed() mislukt: {e}"))
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let prefixed = vec![format!("query: {text}")];
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = model
            .embed(prefixed, None)
            .map_err(|e| anyhow::anyhow!("fastembed embed_query() mislukt: {e}"))?;
        Ok(out.pop().unwrap_or_default())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_name(&self) -> &str {
        &self.model_name
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

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    }

    /// Downloadt en laadt het echte model (~118 MB, eerste keer) — bewust
    /// niet standaard aan: kost tijd en netwerktoegang. Draai expliciet met
    /// `cargo test --release -- --ignored fastembed` om te bewijzen dat de
    /// integratie echt werkt, niet alleen compileert.
    #[test]
    #[ignore = "downloadt een echt model (~118 MB) en heeft internet nodig"]
    fn fastembed_herkent_verwante_tekst_niet_alleen_letterlijke_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let provider = FastEmbedProvider::new(dir.path().to_path_buf())
            .expect("model laden/downloaden mislukt — internet beschikbaar?");

        assert_eq!(provider.dimensions(), 384);
        assert_eq!(provider.model_name(), "intfloat/multilingual-e5-small");

        // Twee verschillende formuleringen van hetzelfde idee — dit is
        // precies waar de mock-provider faalt (§ eerdere review): die hasht
        // de hele string en geeft twee oncorreleerde vectoren. Een écht
        // model moet deze duidelijk hoger scoren dan een ongerelateerd paar.
        let docs = provider
            .embed(&[
                "de rekening moet voor vrijdag betaald worden".to_string(),
                "graag het factuurbedrag deze week nog overmaken".to_string(),
                "de kat slaapt de hele dag op de vensterbank".to_string(),
            ])
            .unwrap();
        assert_eq!(docs.len(), 3);
        assert!(docs.iter().all(|v| v.len() == 384));

        let verwant = cosine(&docs[0], &docs[1]);
        let onverwant = cosine(&docs[0], &docs[2]);
        assert!(
            verwant > onverwant,
            "verwante tekst (score {verwant}) zou hoger moeten scoren dan \
             onverwante tekst (score {onverwant})"
        );
        assert!(verwant > 0.7, "verwachtte sterke gelijkenis, kreeg {verwant}");

        // Een zoekvraag moet het best passende document vinden via
        // embed_query — dit oefent het "query: " vs "passage: "-voorvoegsel
        // uit, niet alleen embed() met hetzelfde voorvoegsel aan beide kanten.
        let query_emb = provider.embed_query("wanneer moet de factuur betaald zijn").unwrap();
        let score_financieel = cosine(&query_emb, &docs[0]);
        let score_kat = cosine(&query_emb, &docs[2]);
        assert!(
            score_financieel > score_kat,
            "zoekvraag over een factuur zou het factuur-document (score \
             {score_financieel}) beter moeten matchen dan de kat (score {score_kat})"
        );
    }
}
