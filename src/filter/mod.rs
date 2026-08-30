//! Het ruisfilter, in vier lagen.
//!
//! 1. **Poort** — voordat er iets gebeurt: ben je idle, is dit een app die we
//!    nooit vastleggen, staat er "Incognito" in de titel?
//! 2. **Beeld** — is dit frame perceptueel gelijk aan het vorige? Dan slaan we
//!    de dure OCR over en verlengen we alleen de duur.
//! 3. **Tekst** — vaste UI-elementen (menubalken, tabtitels, statusbalk) leert
//!    het filter per app af en houdt ze uit de zoekindex. Daarna scoren we of
//!    wat overblijft op taal lijkt.
//! 4. **Redactie** — gevoelige patronen eruit voordat er iets wordt opgeslagen.

pub mod dedupe;
pub mod privacy;
pub mod text;

use crate::capture::WindowInfo;
use crate::config::FilterConfig;
use anyhow::Result;
use privacy::{Denylist, Redactor};
use std::collections::{HashMap, HashSet};

/// Eén geleerde regel: (hash, de regel zelf, in hoeveel frames hij voorkwam).
pub type BoilerplateLine = (u64, String, u32);
/// Alles wat we van één app onthouden: (app-sleutel, aantal frames, regels).
pub type BoilerplateSnapshot = (String, u32, Vec<BoilerplateLine>);

/// Bovengrens op het aantal regels dat we per app onthouden, zodat een app die
/// eindeloos nieuwe regels produceert het geheugen niet opeet.
const MAX_LINES_PER_APP: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Proceed,
    Skip(SkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    NoWindow,
    Idle,
    DeniedApp,
    DeniedTitle,
}

impl SkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkipReason::NoWindow => "geen actief venster",
            SkipReason::Idle => "idle",
            SkipReason::DeniedApp => "app op denylist",
            SkipReason::DeniedTitle => "titel op denylist",
        }
    }
}

/// Resultaat van de tekstlaag.
#[derive(Debug, Clone, Default)]
pub struct TextAnalysis {
    /// Alles wat OCR zag, opgeschoond en geredigeerd. Dit bewaren we.
    pub full_text: String,
    /// Zonder terugkerende UI-elementen. Dit gaat naar de zoekindex.
    pub index_text: String,
    /// Hoe sterk `index_text` op geschreven taal lijkt (0..1).
    pub quality: f32,
    /// Aantal regels dat als boilerplate is herkend.
    pub boilerplate_lines: usize,
    /// Bevat dit frame dezelfde woordenschat als het vorige van deze app?
    pub same_as_previous: bool,
}

impl TextAnalysis {
    pub fn char_len(&self) -> usize {
        self.index_text.chars().count()
    }
}

/// Wat het filter per app onthoudt.
#[derive(Default)]
struct AppState {
    last_tokens: HashSet<String>,
    frames: u32,
    /// regel-hash -> (regel, aantal frames waarin hij voorkwam)
    line_hits: HashMap<u64, (String, u32)>,
    dirty: bool,
}

pub struct NoiseFilter {
    cfg: FilterConfig,
    denylist: Denylist,
    redactor: Redactor,
    /// Tekstgeheugen per app.
    apps: HashMap<String, AppState>,
    /// Laatste beeldhash per app én monitor: op twee schermen wisselen de
    /// frames elkaar af, dus één hash per app zou nooit een duplicaat vinden.
    frame_hashes: HashMap<String, u64>,
}

impl NoiseFilter {
    pub fn new(cfg: FilterConfig) -> Result<Self> {
        let denylist = Denylist::new(&cfg.app_denylist, &cfg.title_denylist)?;
        let redactor = Redactor::new(cfg.redact, &cfg.redact_extra)?;
        Ok(Self {
            cfg,
            denylist,
            redactor,
            apps: HashMap::new(),
            frame_hashes: HashMap::new(),
        })
    }

    /// Herstelt eerder geleerde boilerplate na een herstart, zodat het filter
    /// niet elke keer opnieuw moet leren wat de menubalk van je editor is.
    pub fn restore(&mut self, app_key: &str, frames: u32, lines: Vec<BoilerplateLine>) {
        let state = self.apps.entry(app_key.to_string()).or_default();
        state.frames = frames;
        for (hash, line, hits) in lines {
            state.line_hits.insert(hash, (line, hits));
        }
    }

    /// Apps waarvan de boilerplate-telling sinds de vorige keer is gewijzigd.
    pub fn drain_dirty(&mut self) -> Vec<BoilerplateSnapshot> {
        let mut out = Vec::new();
        for (key, state) in self.apps.iter_mut() {
            if !state.dirty {
                continue;
            }
            state.dirty = false;
            let lines = state
                .line_hits
                .iter()
                .map(|(h, (line, hits))| (*h, line.clone(), *hits))
                .collect();
            out.push((key.clone(), state.frames, lines));
        }
        out
    }

    /// Laag 1: mag dit venster überhaupt worden vastgelegd?
    ///
    /// `idle_after` komt uit de capture-config; het filter kent alleen de vraag
    /// of idle-frames overgeslagen moeten worden, niet de drempel zelf.
    pub fn gate(&self, window: Option<&WindowInfo>, idle_secs: u64, idle_after: u64) -> Gate {
        let Some(window) = window else {
            return Gate::Skip(SkipReason::NoWindow);
        };
        if self.cfg.skip_when_idle && idle_secs >= idle_after {
            return Gate::Skip(SkipReason::Idle);
        }
        if self.denylist.blocks_app(&window.app_key()) {
            return Gate::Skip(SkipReason::DeniedApp);
        }
        if self.denylist.blocks_title(&window.title) {
            return Gate::Skip(SkipReason::DeniedTitle);
        }
        Gate::Proceed
    }

    /// Laag 2: is dit frame perceptueel gelijk aan het vorige op dit scherm?
    ///
    /// `scope` moet app én monitor omvatten. Werkt de opgeslagen hash bij, dus
    /// roep dit precies één keer per frame aan.
    pub fn frame_is_duplicate(&mut self, scope: &str, hash: u64) -> bool {
        let duplicate = match self.frame_hashes.get(scope) {
            Some(prev) => dedupe::hamming(*prev, hash) <= self.cfg.phash_threshold,
            None => false,
        };
        self.frame_hashes.insert(scope.to_string(), hash);
        duplicate
    }

    /// Laag 3 en 4: opschonen, boilerplate wegstrepen, scoren, redigeren.
    pub fn analyze_text(&mut self, app_key: &str, raw_lines: &[String]) -> TextAnalysis {
        let lines = text::normalize(raw_lines);
        if lines.is_empty() {
            return TextAnalysis::default();
        }

        let boilerplate = self.boilerplate_hashes(app_key);
        let mut kept = Vec::with_capacity(lines.len());
        let mut dropped = 0usize;

        for line in &lines {
            if boilerplate.contains(&text::line_hash(line)) {
                dropped += 1;
            } else {
                kept.push(line.as_str());
            }
        }

        self.record_lines(app_key, &lines);

        let full_text = self.redactor.apply(&lines.join("\n"));
        let index_text = self.redactor.apply(&kept.join("\n"));
        let quality = text::quality(&index_text);

        let new_tokens = text::tokens(&index_text);
        let state = self.apps.entry(app_key.to_string()).or_default();
        let same_as_previous = !state.last_tokens.is_empty()
            && text::jaccard(&state.last_tokens, &new_tokens) >= self.cfg.text_similarity;
        state.last_tokens = new_tokens;

        TextAnalysis {
            full_text,
            index_text,
            quality,
            boilerplate_lines: dropped,
            same_as_previous,
        }
    }

    /// Regels die vaak genoeg terugkomen om als vaste UI te gelden.
    fn boilerplate_hashes(&self, app_key: &str) -> HashSet<u64> {
        let Some(state) = self.apps.get(app_key) else {
            return HashSet::new();
        };
        // Onder een minimum aantal frames is elke conclusie toeval.
        if state.frames < self.cfg.boilerplate_min_frames {
            return HashSet::new();
        }
        let threshold = (state.frames as f32 * self.cfg.boilerplate_ratio).ceil() as u32;
        state
            .line_hits
            .iter()
            .filter(|(_, (_, hits))| *hits >= threshold)
            .map(|(hash, _)| *hash)
            .collect()
    }

    fn record_lines(&mut self, app_key: &str, lines: &[String]) {
        let state = self.apps.entry(app_key.to_string()).or_default();
        state.frames = state.frames.saturating_add(1);
        state.dirty = true;

        for line in lines {
            let hash = text::line_hash(line);
            match state.line_hits.get_mut(&hash) {
                Some((_, hits)) => *hits = hits.saturating_add(1),
                None => {
                    if state.line_hits.len() >= MAX_LINES_PER_APP {
                        evict_rarest(&mut state.line_hits);
                    }
                    state.line_hits.insert(hash, (line.clone(), 1));
                }
            }
        }
    }
}

/// Gooit de helft van de zeldzaamste regels weg. Regels die maar één keer
/// voorkwamen zijn per definitie geen boilerplate, dus dat kost geen kennis.
fn evict_rarest(map: &mut HashMap<u64, (String, u32)>) {
    let mut counts: Vec<(u64, u32)> = map.iter().map(|(h, (_, n))| (*h, *n)).collect();
    counts.sort_unstable_by_key(|(_, n)| *n);
    for (hash, _) in counts.into_iter().take(map.len() / 2) {
        map.remove(&hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FilterConfig;

    fn filter() -> NoiseFilter {
        NoiseFilter::new(FilterConfig {
            boilerplate_min_frames: 3,
            boilerplate_ratio: 0.6,
            ..Default::default()
        })
        .unwrap()
    }

    fn window(exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            title: title.into(),
            exe: exe.into(),
            exe_path: format!("C:\\{exe}"),
            pid: 1,
            hwnd: 0,
        }
    }

    #[test]
    fn denylist_blokkeert_wachtwoordkluis() {
        let f = filter();
        let gate = f.gate(Some(&window("bitwarden.exe", "Kluis")), 0, 90);
        assert_eq!(gate, Gate::Skip(SkipReason::DeniedApp));
    }

    #[test]
    fn gewone_app_mag_door() {
        let f = filter();
        assert_eq!(f.gate(Some(&window("code.exe", "main.rs")), 0, 90), Gate::Proceed);
    }

    #[test]
    fn geen_venster_wordt_overgeslagen() {
        assert_eq!(filter().gate(None, 0, 90), Gate::Skip(SkipReason::NoWindow));
    }

    #[test]
    fn identiek_frame_geldt_als_duplicaat() {
        let mut f = filter();
        assert!(!f.frame_is_duplicate("code@1", 0xABCD));
        assert!(f.frame_is_duplicate("code@1", 0xABCD));
        assert!(!f.frame_is_duplicate("code@1", 0x1234_5678_9ABC_DEF0));
    }

    #[test]
    fn twee_schermen_dedupliceren_los_van_elkaar() {
        let mut f = filter();
        // Afwisselend scherm 1 en scherm 2, elk met een eigen stilstaand beeld.
        assert!(!f.frame_is_duplicate("code@1", 0xAAAA));
        assert!(!f.frame_is_duplicate("code@2", 0xBBBB));
        assert!(f.frame_is_duplicate("code@1", 0xAAAA));
        assert!(f.frame_is_duplicate("code@2", 0xBBBB));
    }

    #[test]
    fn terugkerende_menubalk_verdwijnt_uit_de_index() {
        let mut f = filter();
        let chrome = "Bestand Bewerken Beeld Help".to_string();

        // Vier frames met dezelfde menubalk en steeds andere inhoud.
        for i in 0..4 {
            let lines = vec![chrome.clone(), format!("Unieke inhoud nummer {i} hierzo")];
            let out = f.analyze_text("editor", &lines);
            if i < 3 {
                // Nog te weinig frames om iets te concluderen.
                assert!(out.index_text.contains("Bestand"), "frame {i}: {out:?}");
            }
        }

        let out = f.analyze_text("editor", &[chrome.clone(), "Nieuwe alinea tekst".into()]);
        assert_eq!(out.boilerplate_lines, 1, "{out:?}");
        assert!(!out.index_text.contains("Bestand"), "{out:?}");
        // De volledige tekst houdt de menubalk wel, voor als je terugkijkt.
        assert!(out.full_text.contains("Bestand"));
    }

    #[test]
    fn zelfde_inhoud_wordt_als_herhaling_gemarkeerd() {
        let mut f = filter();
        let lines = vec!["De offerte voor klant Jansen staat klaar ter controle".into()];
        let first = f.analyze_text("outlook", &lines);
        assert!(!first.same_as_previous);
        let second = f.analyze_text("outlook", &lines);
        assert!(second.same_as_previous);
    }

    #[test]
    fn redactie_werkt_op_beide_teksten() {
        let mut f = filter();
        let lines = vec!["mijn password: zeergeheimding hier".into()];
        let out = f.analyze_text("notepad", &lines);
        assert!(!out.full_text.contains("zeergeheimding"), "{out:?}");
        assert!(!out.index_text.contains("zeergeheimding"), "{out:?}");
    }
}
