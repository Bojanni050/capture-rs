//! Tekstruis: opschonen, beoordelen en vergelijken van OCR-uitvoer.
//!
//! OCR op een videospeler of een spel levert korte losse fragmenten met veel
//! rare tekens op. OCR op een document levert echte zinnen op. Het verschil
//! is meetbaar, en dat verschil bepaalt of we de tekst vertrouwen of
//! terugvallen op het beeld.

use std::collections::HashSet;

/// Trimt, plakt witruimte samen en gooit lege en dubbele regels weg.
pub fn normalize(lines: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(lines.len());

    for line in lines {
        let cleaned = collapse_whitespace(line);
        if cleaned.is_empty() {
            continue;
        }
        // Losse leestekens en enkele karakters dragen geen informatie.
        if cleaned.chars().count() < 2 || !cleaned.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        if seen.insert(cleaned.clone()) {
            out.push(cleaned);
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// Score van 0 tot 1: hoe sterk lijkt dit op geschreven taal?
///
/// Drie signalen, elk met een eigen gewicht:
/// - aandeel alfanumerieke tekens (OCR-ruis zit vol met `|`, `~`, `»`)
/// - aandeel woorden dat er als een echt woord uitziet
/// - afwezigheid van losse-karakter-rommel
pub fn quality(text: &str) -> f32 {
    let significant: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    if significant.is_empty() {
        return 0.0;
    }

    let alnum = significant.iter().filter(|c| c.is_alphanumeric()).count();
    let alnum_ratio = alnum as f32 / significant.len() as f32;

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return 0.0;
    }

    let wordish = words.iter().filter(|w| looks_like_word(w)).count();
    let word_ratio = wordish as f32 / words.len() as f32;

    let junk = words
        .iter()
        .filter(|w| w.chars().count() == 1 && !w.chars().all(|c| c.is_alphanumeric()))
        .count();
    let junk_ratio = junk as f32 / words.len() as f32;

    (0.35 * alnum_ratio + 0.50 * word_ratio + 0.15 * (1.0 - junk_ratio)).clamp(0.0, 1.0)
}

/// Minstens drie tekens en overwegend letters — geen `#4,` of `||~`.
fn looks_like_word(w: &str) -> bool {
    let chars: Vec<char> = w.chars().collect();
    if chars.len() < 3 {
        return false;
    }
    let alpha = chars.iter().filter(|c| c.is_alphabetic()).count();
    alpha * 10 >= chars.len() * 7
}

/// Tokens voor het vergelijken van twee frames, kleine letters en zonder
/// leestekens.
pub fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Jaccard-overlap: 1.0 betekent dezelfde woordenschat, dus vrijwel zeker
/// hetzelfde scherm ondanks een paar gewijzigde pixels.
pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 1.0;
    }
    intersection as f32 / union as f32
}

/// Stabiele hash voor boilerplate-boekhouding (FNV-1a, 64 bits).
pub fn line_hash(line: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in line.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echte_zinnen_scoren_hoog() {
        let text = "De vergadering van dinsdag gaat niet door omdat de zaal \
                    bezet is door een andere afdeling.";
        assert!(quality(text) > 0.75, "score was {}", quality(text));
    }

    #[test]
    fn broncode_scoort_ruim_boven_de_drempel() {
        let text = "pub fn recognize(&self, image: &RgbaImage) -> Result<RawOcr> {\n\
                    let (width, height) = image.dimensions();";
        assert!(quality(text) > 0.45, "score was {}", quality(text));
    }

    #[test]
    fn ocr_ruis_scoort_laag() {
        let text = "|| ~ 4l . ,, |] 7 : ' ~~ [] 3 . |";
        assert!(quality(text) < 0.38, "score was {}", quality(text));
    }

    #[test]
    fn leeg_is_nul() {
        assert_eq!(quality("   "), 0.0);
    }

    #[test]
    fn normalize_gooit_dubbele_en_lege_regels_weg() {
        let input = vec![
            "  Hallo   wereld ".to_string(),
            "Hallo wereld".to_string(),
            "".to_string(),
            "|".to_string(),
            "Tweede regel".to_string(),
        ];
        assert_eq!(normalize(&input), vec!["Hallo wereld", "Tweede regel"]);
    }

    #[test]
    fn jaccard_herkent_hetzelfde_scherm() {
        let a = tokens("factuur bedrag klant datum betaling");
        let b = tokens("factuur bedrag klant datum betaling");
        assert_eq!(jaccard(&a, &b), 1.0);

        let c = tokens("compleet andere woorden hierzo staan");
        assert!(jaccard(&a, &c) < 0.2);
    }
}
