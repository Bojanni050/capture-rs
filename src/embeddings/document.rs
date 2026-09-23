//! Deterministische semantic-document constructie uit SQLite captures.

use serde::{Deserialize, Serialize};

/// Eén semantisch document — de eenheid die we embedden.
///
/// Bewust géén LLM-samenvatting: deterministisch uit `index_text` + context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticDocument {
    pub id: String, // uuid v4
    pub content: String,
    pub content_hash: String, // FNV-64 hex van content — dedup
    pub application: String,
    pub title: String,
    pub start_time: i64,
    pub end_time: i64,
    pub source_capture_ids: Vec<i64>,
    pub source: String, // uia/ocr/mixed/none
    pub has_frame: bool,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub created_at: i64,
}

/// Ruwe capture rij zoals we hem uit SQLite lezen voor document bouw.
#[derive(Debug, Clone)]
pub struct CaptureRow {
    pub id: i64,
    pub ts: i64,
    pub app: String,
    pub title: String,
    pub source: String,
    pub has_frame: bool,
    pub index_text: String, // al geredigeerd via filter
}

/// Bouwt semantic documents per segment-window.
///
/// Groepeert opeenvolgende captures met zelfde `app+title` binnen `window_secs`.
/// Concateneert `index_text` deterministisch, kapt af op `max_chars`, dedupt via hash.
pub fn build_documents(
    captures: &[CaptureRow],
    window_secs: i64,
    max_chars: usize,
    min_chars: usize,
    model: &str,
    dims: usize,
) -> Vec<SemanticDocument> {
    if captures.is_empty() {
        return Vec::new();
    }
    let mut sorted = captures.to_vec();
    sorted.sort_by_key(|c| c.ts);

    let mut out = Vec::new();
    let mut cur_app = String::new();
    let mut cur_title = String::new();
    let mut cur_start = 0i64;
    let mut cur_end = 0i64;
    let mut cur_ids = Vec::new();
    let mut cur_parts: Vec<String> = Vec::new();
    let mut cur_len = 0usize;
    let mut cur_source = String::from("mixed");
    let mut cur_has_frame = false;

    let flush = |app: String,
                 title: String,
                 start: i64,
                 end: i64,
                 ids: Vec<i64>,
                 parts: Vec<String>,
                 source: String,
                 has_frame: bool,
                 out: &mut Vec<SemanticDocument>| {
        if ids.is_empty() {
            return;
        }
        let content = build_content(&app, &title, &parts, max_chars);
        if content.chars().count() < min_chars {
            return;
        }
        let hash = content_hash(&content);
        // Dedup binnen batch: zelfde hash niet nogmaals.
        if out.iter().any(|d: &SemanticDocument| d.content_hash == hash) {
            return;
        }
        out.push(SemanticDocument {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            content_hash: hash,
            application: app,
            title,
            start_time: start,
            end_time: end,
            source_capture_ids: ids,
            source,
            has_frame,
            embedding_model: model.to_string(),
            embedding_dimensions: dims,
            created_at: chrono::Utc::now().timestamp(),
        });
    };

    let mut prev_source: Option<String> = None;

    for cap in sorted {
        let txt = cap.index_text.trim();
        if txt.is_empty() || txt.chars().count() < min_chars {
            continue;
        }
        let is_new_group = cur_ids.is_empty()
            || cap.app != cur_app
            || cap.title != cur_title
            || (cap.ts - cur_end) > window_secs
            || cur_len + txt.len() + 2 > max_chars;

        if is_new_group && !cur_ids.is_empty() {
            flush(
                cur_app.clone(),
                cur_title.clone(),
                cur_start,
                cur_end,
                std::mem::take(&mut cur_ids),
                std::mem::take(&mut cur_parts),
                cur_source.clone(),
                cur_has_frame,
                &mut out,
            );
            cur_len = 0;
            cur_has_frame = false;
            prev_source = None;
        }
        if cur_ids.is_empty() {
            cur_app = cap.app.clone();
            cur_title = cap.title.clone();
            cur_start = cap.ts;
            cur_source = cap.source.clone();
        } else if prev_source.as_deref() != Some(&cap.source) {
            cur_source = "mixed".into();
        }
        cur_end = cap.ts;
        cur_ids.push(cap.id);
        // Dedup identieke regels binnen document (behoud volgorde)
        if !cur_parts.iter().any(|p| p == txt) {
            cur_len += txt.len() + 1;
            cur_parts.push(txt.to_string());
        }
        cur_has_frame |= cap.has_frame;
        prev_source = Some(cap.source.clone());
    }
    if !cur_ids.is_empty() {
        flush(
            cur_app, cur_title, cur_start, cur_end, cur_ids, cur_parts, cur_source, cur_has_frame, &mut out,
        );
    }
    out
}

fn build_content(app: &str, title: &str, parts: &[String], max_chars: usize) -> String {
    // Deterministisch: app + title + context, geen LLM.
    let header = format!("Application: {app}\nWindow: {title}\nContext:\n");
    let mut content = header;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            content.push_str("\n---\n");
        }
        content.push_str(p);
        if content.chars().count() >= max_chars {
            break;
        }
    }
    // Kapt af op chars, niet bytes.
    if content.chars().count() > max_chars {
        content = content.chars().take(max_chars).collect();
    }
    content
}

pub fn content_hash(content: &str) -> String {
    // FNV-1a 64 hex — stabiel, snel, geen extra crate.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in content.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: i64, ts: i64, app: &str, txt: &str) -> CaptureRow {
        CaptureRow {
            id,
            ts,
            app: app.into(),
            title: "win".into(),
            source: "uia".into(),
            has_frame: false,
            index_text: txt.into(),
        }
    }

    #[test]
    fn deterministische_hash() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("world"));
    }

    #[test]
    fn groepeert_binnen_window() {
        let caps = vec![
            cap(1, 1000, "code", "working on capture uia pipeline"),
            cap(2, 1005, "code", "investigating semantic embeddings architecture"),
            cap(3, 1200, "code", "another app later but same window"),
        ];
        let docs = build_documents(&caps, 120, 4000, 10, "mock", 384);
        // 1 en 2 binnen 120s → 1 doc, 3 buiten window → 2e doc
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].source_capture_ids, vec![1, 2]);
        assert_eq!(docs[1].source_capture_ids, vec![3]);
    }

    #[test]
    fn filtert_korte_teksten() {
        let caps = vec![cap(1, 1000, "code", "hi"), cap(2, 1001, "code", "ok")];
        let docs = build_documents(&caps, 120, 4000, 10, "mock", 384);
        assert!(docs.is_empty());
    }

    #[test]
    fn provenance_behoud() {
        let caps = vec![cap(42, 1000, "code", "meaningful content here please")];
        let docs = build_documents(&caps, 120, 4000, 10, "mock", 384);
        assert_eq!(docs[0].source_capture_ids, vec![42]);
        assert_eq!(docs[0].application, "code");
    }

    #[test]
    fn max_chars_kapt_af() {
        let caps = vec![cap(1, 1000, "code", &"a".repeat(5000))];
        let docs = build_documents(&caps, 120, 100, 10, "mock", 384);
        assert!(docs[0].content.chars().count() <= 100);
    }
}
