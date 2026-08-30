//! Configuratie: TOML-bestand met verstandige defaults.
//!
//! Alles wat je zou willen tunen (sample-tempo, filterdrempels, retentie,
//! privacy-denylist) staat hier, zodat de rest van de code geen magische
//! getallen bevat.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Waar frames en de database landen. Leeg = %LOCALAPPDATA%\ChronicleCapture.
    pub data_dir: Option<PathBuf>,
    pub capture: CaptureConfig,
    pub uia: UiaConfig,
    pub ocr: OcrConfig,
    pub filter: FilterConfig,
    pub storage: StorageConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    /// Seconden tussen twee samples terwijl je actief bent.
    pub interval_secs: f64,
    /// Seconden tussen twee samples terwijl je idle bent (0 = helemaal niet).
    pub idle_interval_secs: f64,
    /// Vanaf hoeveel seconden zonder toetsenbord/muis je als idle telt.
    pub idle_after_secs: u64,
    /// "primary", "all", of een monitor-index ("0", "1", ...).
    pub monitor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiaConfig {
    /// UI Automation als primaire tekstbron gebruiken.
    pub enabled: bool,
    /// Gebruik event-driven UIA in plaats van polling.
    pub event_driven: bool,
    /// Minder tekens dan dit uit de boom → doorschuiven naar OCR.
    pub min_text_len: usize,
    /// Bovengrens op het aantal knopen dat we per venster uitlezen.
    pub max_elements: usize,
    /// Zoveel wachten we op één leesactie; daarna gaat OCR verder.
    pub timeout_ms: u64,
    /// Na zoveel teleurstellingen op rij gaat UIA voor die app uit.
    pub failures_before_skip: u32,
    /// Hoe lang die pauze duurt voordat de app weer een kans krijgt.
    pub retry_after_secs: u64,
    /// Apps waarvoor we UIA nooit proberen (kleine letters, zonder .exe).
    pub app_denylist: Vec<String>,
}

impl Default for UiaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Event-driven is nu een stub (zie src/uia/events.rs) — alleen
            // bookkeeping, geen echte COM handlers. Default uit tot de
            // implementatie met #[implement(IUIAutomation...Handler)] af is.
            event_driven: false,
            // Hoger dan de OCR-drempel: een boom die alleen "Bestand" en "OK"
            // oplevert is geen inhoud, en dan wil je alsnog OCR proberen.
            // Chromium-browsers schakelen accessibility geleidelijk in, dus de
            // eerste minuten geven ze alleen hun eigen menubalk — die drempel
            // houdt die magere oogst uit je archief.
            min_text_len: 120,
            max_elements: 1_500,
            // Gemeten op een 2560x1440-scherm: eenvoudige vensters zijn in
            // 250-400 ms klaar, een volle Electron- of browserboom kost 1,5-2,5 s.
            // Dat past nog binnen een tik van 4 s, en wat er niet in past valt
            // gewoon terug op OCR (~300-500 ms voor een heel scherm).
            timeout_ms: 2_500,
            failures_before_skip: 3,
            retry_after_secs: 600,
            app_denylist: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrConfig {
    pub enabled: bool,
    /// BCP-47 taal, bv. "nl-NL" of "en-US". Leeg = talen uit je Windows-profiel.
    pub language: Option<String>,
    /// Minimum aantal tekens voordat OCR-tekst als inhoud telt.
    pub min_text_len: usize,
    /// Minimum kwaliteitsscore (0..1) voordat OCR-tekst wordt vertrouwd.
    pub min_quality: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterConfig {
    /// Max Hamming-afstand tussen twee dHashes om ze gelijk te noemen (0..64).
    pub phash_threshold: u32,
    /// Jaccard-overlap waarboven twee OCR-teksten als hetzelfde tellen.
    pub text_similarity: f32,
    /// Processen die nooit gesampled worden (kleine letters, zonder .exe).
    pub app_denylist: Vec<String>,
    /// Reguliere expressies op de venstertitel; match = overslaan.
    pub title_denylist: Vec<String>,
    /// Sla helemaal niets op zolang je idle bent.
    pub skip_when_idle: bool,
    /// Vanaf hoeveel frames per app we boilerplate-detectie vertrouwen.
    pub boilerplate_min_frames: u32,
    /// Regel die in >= dit aandeel van de frames van een app voorkomt = chrome.
    pub boilerplate_ratio: f32,
    /// Gevoelige patronen (creditcard, IBAN, tokens) vervangen door [REDACTED].
    pub redact: bool,
    /// Extra eigen redactie-regexes.
    pub redact_extra: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// "fallback" = alleen als OCR faalt, "always", of "never".
    pub keep_frames: FramePolicy,
    /// JPEG-kwaliteit voor bewaarde frames (1..100).
    pub frame_quality: u8,
    /// Frames worden hiernaartoe geschaald (breedte in px, 0 = niet schalen).
    pub frame_max_width: u32,
    /// Captures ouder dan dit worden verwijderd (0 = nooit).
    pub retention_days: u32,
    /// Frame-afbeeldingen ouder dan dit worden verwijderd, tekst blijft.
    pub frame_retention_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FramePolicy {
    Never,
    Fallback,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            interval_secs: 4.0,
            idle_interval_secs: 60.0,
            idle_after_secs: 90,
            monitor: "primary".into(),
        }
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            language: None,
            min_text_len: 24,
            min_quality: 0.38,
        }
    }
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            phash_threshold: 4,
            text_similarity: 0.93,
            app_denylist: [
                "keepass",
                "keepassxc",
                "bitwarden",
                "1password",
                "lastpass",
                "protonpass",
                "dashlane",
                "enpass",
                "credentialuibroker",
                "consent",
                "lsass",
                "logonui",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            title_denylist: [
                r"(?i)\bInPrivate\b",
                r"(?i)\bIncognito\b",
                r"(?i)Private Browsing",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            skip_when_idle: true,
            boilerplate_min_frames: 25,
            boilerplate_ratio: 0.6,
            redact: true,
            redact_extra: Vec::new(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            keep_frames: FramePolicy::Fallback,
            frame_quality: 72,
            frame_max_width: 1600,
            retention_days: 45,
            frame_retention_days: 10,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: "127.0.0.1".into(),
            port: 7331,
        }
    }
}

impl Config {
    /// Standaardlocatie van het configuratiebestand.
    pub fn default_path() -> Result<PathBuf> {
        Ok(app_root()?.join("chronicle.toml"))
    }

    /// Laadt de config; ontbreekt het bestand, dan gelden de defaults.
    pub fn load(path: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_path()?,
        };
        if !path.exists() {
            let cfg = Self::default();
            cfg.validate()?;
            return Ok((cfg, path));
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("config lezen mislukt: {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("config parsen mislukt: {}", path.display()))?;
        cfg.validate()?;
        Ok((cfg, path))
    }

    /// Controleert bereiken; voorkomt dat een tikfout je capture stillegt.
    pub fn validate(&self) -> Result<()> {
        use anyhow::anyhow;
        if self.capture.interval_secs < 0.1 || self.capture.interval_secs > 3600.0 {
            return Err(anyhow!(
                "capture.interval_secs moet tussen 0.1 en 3600 liggen (gevonden {})",
                self.capture.interval_secs
            ));
        }
        if self.capture.idle_interval_secs < 0.0 || self.capture.idle_interval_secs > 3600.0 {
            return Err(anyhow!(
                "capture.idle_interval_secs moet tussen 0 en 3600 liggen"
            ));
        }
        if self.filter.phash_threshold > 64 {
            return Err(anyhow!(
                "filter.phash_threshold moet 0..64 zijn (gevonden {})",
                self.filter.phash_threshold
            ));
        }
        if !(0.0..=1.0).contains(&self.filter.text_similarity) {
            return Err(anyhow!("filter.text_similarity moet 0..1 zijn"));
        }
        if !(0.0..=1.0).contains(&self.filter.boilerplate_ratio) {
            return Err(anyhow!("filter.boilerplate_ratio moet 0..1 zijn"));
        }
        if self.ocr.min_quality < 0.0 || self.ocr.min_quality > 1.0 {
            return Err(anyhow!("ocr.min_quality moet 0..1 zijn"));
        }
        if !(1..=100).contains(&self.storage.frame_quality) {
            return Err(anyhow!(
                "storage.frame_quality moet 1..100 zijn (gevonden {})",
                self.storage.frame_quality
            ));
        }
        if self.uia.max_elements < 50 || self.uia.max_elements > 20_000 {
            return Err(anyhow!("uia.max_elements moet 50..20000 zijn"));
        }
        if self.uia.timeout_ms == 0 || self.uia.timeout_ms > 30_000 {
            return Err(anyhow!("uia.timeout_ms moet 1..30000 zijn"));
        }
        Ok(())
    }

    /// Schrijft de huidige config weg, maakt tussenliggende mappen aan.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(path, raw)
            .with_context(|| format!("config schrijven mislukt: {}", path.display()))?;
        Ok(())
    }

    /// Effectieve datamap, aangemaakt als die nog niet bestaat.
    pub fn resolved_data_dir(&self) -> Result<PathBuf> {
        let dir = match &self.data_dir {
            Some(d) => d.clone(),
            None => app_root()?.join("data"),
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("datamap aanmaken mislukt: {}", dir.display()))?;
        Ok(dir)
    }

    pub fn db_path(&self) -> Result<PathBuf> {
        Ok(self.resolved_data_dir()?.join("chronicle.db"))
    }

    pub fn frames_dir(&self) -> Result<PathBuf> {
        let dir = self.resolved_data_dir()?.join("frames");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// %LOCALAPPDATA%\ChronicleCapture (of het platform-equivalent).
fn app_root() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("geen home-map gevonden")?;
    Ok(base.data_local_dir().join("ChronicleCapture"))
}
