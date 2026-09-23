//! Configuratie: TOML-bestand met verstandige defaults.
//!
//! Alles wat je zou willen tunen (sample-tempo, filterdrempels, retentie,
//! privacy-denylist) staat hier, zodat de rest van de code geen magische
//! getallen bevat.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Waarde uit de omgeving, leeg getrimd beschouwd als niet gezet.
fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Waar frames en de database landen. Leeg = %LOCALAPPDATA%\Capture.
    pub data_dir: Option<PathBuf>,
    pub capture: CaptureConfig,
    pub uia: UiaConfig,
    pub ocr: OcrConfig,
    pub filter: FilterConfig,
    pub storage: StorageConfig,
    pub server: ServerConfig,
    pub browser: BrowserConfig,
    pub ship: ShipConfig,
    pub embeddings: EmbeddingsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// Lokale bridge waar de browserextensie domein + wachtwoordveld meldt.
    pub enabled: bool,
    pub port: u16,
    /// Een melding ouder dan dit vertrouwen we niet meer (extensie weg, tab
    /// niet meer actief). De extensie stuurt elke 5 s een hartslag.
    pub max_age_secs: f64,
    /// Procesnamen (kleine letters, zonder .exe) die als browser tellen.
    pub processes: Vec<String>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8765,
            max_age_secs: 15.0,
            processes: ["chrome", "msedge", "firefox", "brave", "opera", "vivaldi"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShipConfig {
    /// Stuur gefilterde tekst naar de Foundation Ingestie Gateway (VPS),
    /// zie docs/foundation-gateway.md. Standaard uit: zonder dit blijft
    /// alles op deze machine.
    pub enabled: bool,
    pub endpoint: String,
    /// Bearer-token; leeg = uit de omgevingsvariabele `CAPTURE_INGEST_TOKEN`
    /// (legacy: `CHRONICLE_INGEST_TOKEN`, `STASH_AUTH_TOKEN`).
    pub auth_token: Option<String>,
    pub interval_secs: f64,
}

impl Default for ShipConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://100.65.0.15:4577/api/ingest/capture".into(),
            auth_token: None,
            interval_secs: 300.0,
        }
    }
}

impl ShipConfig {
    pub fn resolved_token(&self) -> Option<String> {
        self.auth_token
            .clone()
            .filter(|t| !t.trim().is_empty())
            // De CHRONICLE_- en STASH_-namen zijn legacy uit de tijd vóór de
            // rebrand; die blijven werken zodat bestaande deployments niet
            // stilzwijgend stoppen met shippen.
            .or_else(|| env_non_empty("CAPTURE_INGEST_TOKEN"))
            .or_else(|| env_non_empty("CHRONICLE_INGEST_TOKEN"))
            .or_else(|| env_non_empty("STASH_AUTH_TOKEN"))
    }
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
            // Event-driven: via echte COM handlers (AddStructureChanged/
            // AddPropertyChanged) — alleen lezen bij verandering, geen polling.
            event_driven: true,
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
                "capture",
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingsConfig {
    /// Semantische index aan/uit. Uit = capture werkt normaal, alleen vector zoek is unavailable.
    pub enabled: bool,
    /// Provider: "mock" (deterministisch, geen echt model — voor tests en als
    /// veilige default) of "fastembed" (intfloat/multilingual-e5-small,
    /// lokaal via ONNX Runtime; downloadt ~118 MB bij een lege cache).
    pub provider: String,
    /// Model naam voor provenance (bv. intfloat/multilingual-e5-small).
    pub model: String,
    /// Verwachte vector dimensies (voor pgvector schema).
    pub dimensions: usize,
    /// PostgreSQL connectie-string. Leeg = uit env `CAPTURE_POSTGRES_URL` of `DATABASE_URL`.
    pub postgres_url: Option<String>,
    /// Batch grootte voor embedding aanroepen.
    pub batch_size: usize,
    /// Poll interval voor async indexer (secs).
    pub poll_interval_secs: f64,
    /// Max chars per semantic document (afkappen, deterministisch).
    pub max_content_chars: usize,
    /// Min chars om te embedden (filter ruis).
    pub min_chars: usize,
    /// Venster in seconden voor grouping per segment.
    pub window_secs: i64,
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "mock".into(),
            model: "multilingual-e5-small-mock".into(),
            dimensions: 384,
            postgres_url: None,
            batch_size: 32,
            poll_interval_secs: 5.0,
            max_content_chars: 4000,
            min_chars: 40,
            window_secs: 120,
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
        let root = app_root()?;
        // Config van vóór de rebrand meenemen naar de nieuwe bestandsnaam.
        migrate_legacy_file(&root.join(LEGACY_CONFIG), &root.join(CONFIG_NAME));
        Ok(root.join(CONFIG_NAME))
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
        if self.ship.enabled {
            if !(10.0..=86_400.0).contains(&self.ship.interval_secs) {
                return Err(anyhow!("ship.interval_secs moet 10..86400 zijn"));
            }
            if !self.ship.endpoint.starts_with("http://") {
                return Err(anyhow!(
                    "ship.endpoint moet met http:// beginnen (alleen over Tailscale; geen TLS-client ingebouwd)"
                ));
            }
        }
        if self.embeddings.enabled {
            if self.embeddings.dimensions == 0 || self.embeddings.dimensions > 4096 {
                return Err(anyhow!("embeddings.dimensions moet 1..4096 zijn"));
            }
            if self.embeddings.batch_size == 0 || self.embeddings.batch_size > 512 {
                return Err(anyhow!("embeddings.batch_size moet 1..512 zijn"));
            }
            if self.embeddings.max_content_chars < 100 || self.embeddings.max_content_chars > 20000 {
                return Err(anyhow!("embeddings.max_content_chars moet 100..20000 zijn"));
            }
        }
        Ok(())
    }

    /// Opgeloste postgres URL met env fallback.
    pub fn embeddings_postgres_url(&self) -> Option<String> {
        if let Some(url) = &self.embeddings.postgres_url {
            if !url.trim().is_empty() {
                return Some(url.clone());
            }
        }
        env_non_empty("CAPTURE_POSTGRES_URL")
            // Legacy-naam uit de Chronicle-tijd.
            .or_else(|| env_non_empty("CHRONICLE_POSTGRES_URL"))
            .or_else(|| env_non_empty("DATABASE_URL"))
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
        // Bestanden van vóór de rebrand hernoemen, zodat de opgebouwde
        // geschiedenis en de ship-cursor behouden blijven.
        migrate_legacy_file(&dir.join(LEGACY_DB), &dir.join(DB_NAME));
        migrate_legacy_file(&dir.join(LEGACY_LOCK), &dir.join(LOCK_NAME));
        Ok(dir)
    }

    pub fn db_path(&self) -> Result<PathBuf> {
        Ok(self.resolved_data_dir()?.join(DB_NAME))
    }

    pub fn lock_path(&self) -> Result<PathBuf> {
        Ok(self.resolved_data_dir()?.join(LOCK_NAME))
    }

    pub fn frames_dir(&self) -> Result<PathBuf> {
        let dir = self.resolved_data_dir()?.join("frames");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Waar lokale embeddingmodellen gecached worden. `fastembed`'s eigen
    /// default is een relatief pad (`.fastembed_cache`), dus afhankelijk van
    /// de werkmap zou hetzelfde model telkens opnieuw gedownload kunnen
    /// worden. Dit legt het naast de rest van Capture's data vast.
    pub fn models_dir(&self) -> Result<PathBuf> {
        let dir = self.resolved_data_dir()?.join("models");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// %LOCALAPPDATA%\Capture (of het platform-equivalent).
fn app_root() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("geen home-map gevonden")?;
    let current = base.data_local_dir().join(APP_DIR);
    // Map van vóór de rebrand verhuizen, zodat data, config en frames
    // meegaan naar de nieuwe naam.
    migrate_legacy_dir(&base.data_local_dir().join(LEGACY_APP_DIR), &current);
    Ok(current)
}

const APP_DIR: &str = "Capture";
const LEGACY_APP_DIR: &str = "ChronicleCapture";
const CONFIG_NAME: &str = "capture.toml";
const LEGACY_CONFIG: &str = "chronicle.toml";
const DB_NAME: &str = "capture.db";
const LEGACY_DB: &str = "chronicle.db";
const LOCK_NAME: &str = "capture.lock";
const LEGACY_LOCK: &str = "chronicle.lock";

/// Hernoemt een bestand van vóór de rebrand naar de nieuwe naam. Doet niets
/// als het nieuwe bestand al bestaat of het oude ontbreekt, zodat elke aanroep
/// veilig is. Faalt het hernoemen (bv. bestand in gebruik), dan gaat de app
/// gewoon verder met een lege stand — liever dat dan een crash bij het starten.
fn migrate_legacy_file(legacy: &Path, current: &Path) {
    if current.exists() || !legacy.exists() {
        return;
    }
    if let Err(e) = std::fs::rename(legacy, current) {
        tracing::warn!(
            error = %e,
            van = %legacy.display(),
            naar = %current.display(),
            "oude bestand hernoemen naar de nieuwe naam mislukt"
        );
    }
}

/// Zelfde als [`migrate_legacy_file`], maar voor de hele datamap.
fn migrate_legacy_dir(legacy: &Path, current: &Path) {
    if current.exists() || !legacy.exists() {
        return;
    }
    if let Some(parent) = current.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::rename(legacy, current) {
        tracing::warn!(
            error = %e,
            van = %legacy.display(),
            naar = %current.display(),
            "oude datamap hernoemen naar de nieuwe naam mislukt"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oud_bestand_wordt_naar_de_nieuwe_naam_verhuisd() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_DB);
        let current = dir.path().join(DB_NAME);
        std::fs::write(&legacy, "geschiedenis").unwrap();

        migrate_legacy_file(&legacy, &current);

        assert_eq!(std::fs::read_to_string(&current).unwrap(), "geschiedenis");
        assert!(!legacy.exists());
    }

    #[test]
    fn bestaand_nieuw_bestand_wint_het_van_het_oude() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_CONFIG);
        let current = dir.path().join(CONFIG_NAME);
        std::fs::write(&legacy, "oud").unwrap();
        std::fs::write(&current, "nieuw").unwrap();

        migrate_legacy_file(&legacy, &current);

        assert_eq!(std::fs::read_to_string(&current).unwrap(), "nieuw");
    }

    #[test]
    fn ontbrekend_oudbestand_is_geen_fout() {
        let dir = tempfile::tempdir().unwrap();
        // Doet niets, en al zeker geen fout: dit wordt bij elke start geroepen.
        migrate_legacy_file(&dir.path().join(LEGACY_LOCK), &dir.path().join(LOCK_NAME));
        assert!(!dir.path().join(LOCK_NAME).exists());
    }

    #[test]
    fn oude_datamap_wordt_met_zijn_inhoud_verhuisd() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_APP_DIR);
        let current = dir.path().join(APP_DIR);
        std::fs::create_dir_all(legacy.join("data")).unwrap();
        std::fs::write(legacy.join("data").join(LEGACY_DB), "geschiedenis").unwrap();

        migrate_legacy_dir(&legacy, &current);

        assert!(!legacy.exists());
        assert_eq!(
            std::fs::read_to_string(current.join("data").join(LEGACY_DB)).unwrap(),
            "geschiedenis"
        );
        // De bestandsnaam zelf volgt zodra de datamap wordt geopend.
        migrate_legacy_file(
            &current.join("data").join(LEGACY_DB),
            &current.join("data").join(DB_NAME),
        );
        assert_eq!(
            std::fs::read_to_string(current.join("data").join(DB_NAME)).unwrap(),
            "geschiedenis"
        );
    }
}
