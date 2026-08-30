//! Chronicle — houdt bij wat je op je pc doet, met een ruisfilter ervoor.
//!
//! Zie `README.md` voor de opzet; de opnamelus zelf staat in `pipeline.rs` en
//! het filter in `filter/mod.rs`.

mod capture;
mod com;
mod config;
mod filter;
mod ocr;
mod pipeline;
mod server;
mod store;
mod uia;

use anyhow::{anyhow, Context, Result};
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};
use config::Config;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use store::{Db, FrameStore, SearchQuery};

#[derive(Parser)]
#[command(
    name = "chronicle",
    version,
    about = "Legt vast wat je op je pc doet: tekst via de accessibility-boom, \
             dan OCR, met beeld als laatste vangnet."
)]
struct Cli {
    /// Alternatief configuratiebestand.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Meer logregels (-v = debug, -vv = trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start de opname (en standaard ook de webinterface).
    Start {
        /// Draai alleen de opname, zonder webserver.
        #[arg(long)]
        no_server: bool,
    },
    /// Alleen de webinterface, zonder op te nemen.
    Serve,
    /// Zoek in wat er is vastgelegd.
    Search {
        /// Zoekterm; laat leeg om simpelweg het nieuwste te zien.
        query: Vec<String>,
        /// Beperk tot één app, bv. `code` of `chrome`.
        #[arg(long)]
        app: Option<String>,
        /// `text` of `image`.
        #[arg(long)]
        kind: Option<String>,
        /// Hoe ver terug, bv. `2h`, `7d`, `30m`.
        #[arg(long, default_value = "7d")]
        since: String,
        #[arg(long, default_value_t = 20)]
        limit: i64,
        /// Toon de volledige tekst in plaats van een fragment.
        #[arg(long)]
        full: bool,
    },
    /// Samenvatting van wat er is vastgelegd en weggefilterd.
    Stats {
        #[arg(long, default_value = "7d")]
        since: String,
    },
    /// Ruim oude gegevens op volgens het retentiebeleid.
    Purge {
        /// Verwijder captures ouder dan dit (standaard: uit de config).
        #[arg(long)]
        older_than: Option<String>,
        /// Verwijder alleen de afbeeldingen ouder dan dit; tekst blijft.
        #[arg(long)]
        frames_older_than: Option<String>,
        /// Zonder deze vlag wordt alleen getoond wát er zou verdwijnen.
        #[arg(long)]
        yes: bool,
        /// Comprimeer de database na afloop.
        #[arg(long)]
        vacuum: bool,
    },
    /// Controleer of alles werkt: OCR, schermen, database, opslag.
    Doctor,
    /// Toon of maak het configuratiebestand.
    Config {
        /// Schrijf een configuratiebestand met alle defaults.
        #[arg(long)]
        init: bool,
    },
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let (cfg, cfg_path) = Config::load(cli.config.as_deref())?;

    match cli.command {
        Command::Start { no_server } => cmd_start(cfg, no_server).await,
        Command::Serve => cmd_serve(cfg).await,
        Command::Search {
            query,
            app,
            kind,
            since,
            limit,
            full,
        } => cmd_search(cfg, query.join(" "), app, kind, &since, limit, full),
        Command::Stats { since } => cmd_stats(cfg, &since),
        Command::Purge {
            older_than,
            frames_older_than,
            yes,
            vacuum,
        } => cmd_purge(cfg, older_than, frames_older_than, yes, vacuum),
        Command::Doctor => cmd_doctor(cfg, &cfg_path),
        Command::Config { init } => cmd_config(cfg, &cfg_path, init),
    }
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
}

/// Opent database en frame-opslag op basis van de config.
fn open_store(cfg: &Config) -> Result<(Arc<Db>, Arc<FrameStore>)> {
    let db = Arc::new(Db::open(&cfg.db_path()?)?);
    let frames = Arc::new(FrameStore::new(
        cfg.frames_dir()?,
        cfg.storage.frame_quality,
        cfg.storage.frame_max_width,
    ));
    Ok((db, frames))
}

async fn cmd_start(cfg: Config, no_server: bool) -> Result<()> {
    let (db, frames) = open_store(&cfg)?;

    if !no_server && cfg.server.enabled {
        let state = server::AppState {
            db: Arc::clone(&db),
            frames: Arc::clone(&frames),
        };
        match server::serve(state, &cfg.server.bind, cfg.server.port).await {
            Ok(addr) => tracing::info!("webinterface op http://{addr}"),
            Err(e) => tracing::error!(error = %e, "webserver kon niet starten"),
        }
    }

    let mut pipeline = pipeline::Pipeline::new(cfg, Arc::clone(&db), Arc::clone(&frames))?;
    tracing::info!(schermen = %pipeline.monitors(), "opname gestart — Ctrl+C om te stoppen");

    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = tx.send(true);
        }
    });

    pipeline.run(rx).await
}

async fn cmd_serve(cfg: Config) -> Result<()> {
    let (db, frames) = open_store(&cfg)?;
    let state = server::AppState { db, frames };
    let addr = server::serve(state, &cfg.server.bind, cfg.server.port).await?;
    println!("Webinterface draait op http://{addr} — Ctrl+C om te stoppen.");
    tokio::signal::ctrl_c().await?;
    Ok(())
}

fn cmd_search(
    cfg: Config,
    query: String,
    app: Option<String>,
    kind: Option<String>,
    since: &str,
    limit: i64,
    full: bool,
) -> Result<()> {
    let (db, _frames) = open_store(&cfg)?;
    let from = Local::now().timestamp() - parse_duration(since)?;

    let hits = db.search(&SearchQuery {
        text: query,
        app,
        kind,
        from: Some(from),
        to: None,
        limit,
        offset: 0,
    })?;

    if hits.is_empty() {
        println!("Niets gevonden.");
        return Ok(());
    }

    let color = std::io::stdout().is_terminal();
    for hit in &hits {
        let when = Local
            .timestamp_opt(hit.ts, 0)
            .single()
            .map(|t| t.format("%d-%m %H:%M").to_string())
            .unwrap_or_default();
        let soort = if hit.kind == "image" {
            "beeld".to_string()
        } else {
            format!("tekst/{}", hit.source)
        };

        println!(
            "\n{when}  {}  [{soort}]  {}",
            hit.app,
            truncate(&hit.title, 60)
        );

        let body = if full {
            db.capture(hit.id)?.map(|c| c.text).unwrap_or_default()
        } else {
            hit.snippet.clone()
        };
        println!("  {}", markeer(&body, color).replace('\n', "\n  "));
        println!("  \u{2192} http://{}:{}/#{}", cfg.server.bind, cfg.server.port, hit.id);
    }
    println!("\n{} resultaten.", hits.len());
    Ok(())
}

fn cmd_stats(cfg: Config, since: &str) -> Result<()> {
    let (db, frames) = open_store(&cfg)?;
    let now = Local::now().timestamp();
    let from = now - parse_duration(since)?;
    let s = db.stats(from, now)?;

    println!("Periode          laatste {since}");
    println!("Vastgelegd       {}", s.captures);
    println!("  via uia        {}", s.uia_captures);
    println!("  via ocr        {}", s.ocr_captures);
    println!("  beeld-fallback {}", s.image_captures);
    if s.text_captures > 0 {
        println!(
            "  uia-aandeel    {:.0}% van de tekst kwam uit de accessibility-boom",
            100.0 * s.uia_captures as f64 / s.text_captures as f64
        );
    }
    println!("Vensters         {}", s.segments);
    println!("Apps             {}", s.apps);
    println!("Tekst            {} tekens geïndexeerd", s.total_chars);
    println!(
        "Frames           {} bestanden, {:.1} MB",
        s.frames_on_disk,
        frames.disk_usage() as f64 / 1_048_576.0
    );

    if let (Some(first), Some(last)) = (s.first_ts, s.last_ts) {
        println!(
            "Bereik           {} tot {}",
            fmt_ts(first),
            fmt_ts(last)
        );
    }

    if !s.top_apps.is_empty() {
        println!("\nMeeste captures:");
        for (app, n) in &s.top_apps {
            println!("  {n:>6}  {app}");
        }
    }

    if !s.skipped.is_empty() {
        println!("\nRuisfilter (sinds het begin):");
        let total: i64 = s.skipped.iter().map(|(_, n)| n).sum();
        for (reason, n) in &s.skipped {
            println!("  {n:>6}  {reason}");
        }
        println!("  {total:>6}  totaal overgeslagen");
    }
    Ok(())
}

fn cmd_purge(
    cfg: Config,
    older_than: Option<String>,
    frames_older_than: Option<String>,
    yes: bool,
    vacuum: bool,
) -> Result<()> {
    let (db, frames) = open_store(&cfg)?;
    let now = Local::now().timestamp();

    let captures_before = match &older_than {
        Some(spec) => Some(now - parse_duration(spec)?),
        None if cfg.storage.retention_days > 0 => {
            Some(now - cfg.storage.retention_days as i64 * 86_400)
        }
        None => None,
    };
    let frames_before = match &frames_older_than {
        Some(spec) => Some(now - parse_duration(spec)?),
        None if cfg.storage.frame_retention_days > 0 => {
            Some(now - cfg.storage.frame_retention_days as i64 * 86_400)
        }
        None => None,
    };

    if captures_before.is_none() && frames_before.is_none() {
        println!("Geen retentie ingesteld; er valt niets op te ruimen.");
        return Ok(());
    }

    let (n_captures, n_frames) = db.purge_preview(captures_before, frames_before)?;

    if !yes {
        println!("Dit zou verdwijnen:");
        if let Some(cutoff) = captures_before {
            println!("  {n_captures} captures van vóór {}", fmt_ts(cutoff));
        }
        if let Some(cutoff) = frames_before {
            println!("  {n_frames} afbeeldingen van vóór {} (tekst blijft)", fmt_ts(cutoff));
        }
        println!("\nVoeg --yes toe om het echt te doen.");
        return Ok(());
    }

    let report = db.purge(captures_before, frames_before)?;
    let mut verwijderd = 0;
    for path in &report.frame_paths {
        if frames.delete(path).is_ok() {
            verwijderd += 1;
        }
    }
    frames.prune_empty_dirs();

    println!(
        "{} captures verwijderd, {} afbeeldingen van schijf.",
        report.captures_deleted, verwijderd
    );

    if vacuum {
        db.vacuum()?;
        println!("Database gecomprimeerd.");
    }
    Ok(())
}

fn cmd_doctor(cfg: Config, cfg_path: &std::path::Path) -> Result<()> {
    println!("Config           {}", cfg_path.display());
    if !cfg_path.exists() {
        println!("                 (bestaat niet; defaults zijn actief)");
    }

    let data_dir = cfg.resolved_data_dir()?;
    println!("Datamap          {}", data_dir.display());

    let db_path = cfg.db_path()?;
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    println!("Database         {} ({:.1} MB)", db_path.display(), db_size as f64 / 1_048_576.0);

    match Db::open(&db_path) {
        Ok(_) => println!("                 schema in orde, FTS5 beschikbaar"),
        Err(e) => println!("  !! database:   {e}"),
    }

    println!();
    match capture::ScreenCapturer::new(&cfg.capture.monitor) {
        Ok(cap) => println!("Schermen         {}", cap.describe()),
        Err(e) => println!("  !! schermen:   {e}"),
    }

    match capture::foreground() {
        Some(w) => println!("Actief venster   {} — {}", w.exe, truncate(&w.title, 50)),
        None => println!("Actief venster   (geen)"),
    }
    println!("Inactief sinds   {} s", capture::idle_seconds());

    println!();
    if let Err(e) = proef_uia(&cfg) {
        println!("  !! UIA:        {e}");
    }

    println!();
    match ocr::available_languages() {
        Ok(langs) if langs.is_empty() => {
            println!("  !! OCR:        geen taalpakketten met OCR gevonden");
        }
        Ok(langs) => println!("OCR-talen        {}", langs.join(", ")),
        Err(e) => println!("  !! OCR:        {e}"),
    }

    // De echte proef: één screenshot door de hele keten halen.
    print!("Proefopname      ");
    match proefopname(&cfg) {
        Ok(msg) => println!("{msg}"),
        Err(e) => println!("mislukt: {e}"),
    }

    Ok(())
}

/// Probeert UI Automation op elk open venster en rapporteert per app.
///
/// Dit beantwoordt de enige vraag die er bij UIA toe doet: welke van jóuw apps
/// vullen hun accessibility-boom, en welke laten je met OCR zitten?
fn proef_uia(cfg: &Config) -> Result<()> {
    if !cfg.uia.enabled {
        println!("UIA              uitgeschakeld in de config");
        return Ok(());
    }

    let reader = uia::reader::UiaReader::new(cfg.uia.max_elements)?;
    let mut windows_seen = capture::top_level_windows();

    // Eén regel per app; het grootste venster van een app is representatief.
    windows_seen.sort_by_key(|w| w.app_key());
    windows_seen.dedup_by(|a, b| a.app_key() == b.app_key());

    println!("UIA per app      (knopen = grootte van de accessibility-boom)");
    println!(
        "  {:<18} {:>7} {:>8} {:>6} {:>6}  oordeel",
        "app", "knopen", "tekens", "doc ms", "boom"
    );

    for window in windows_seen.iter().take(20) {
        let read = reader.read_window(windows::Win32::Foundation::HWND(
            window.hwnd as *mut core::ffi::c_void,
        ));

        let (elements, chars, doc_ms, tree_ms, oordeel) = match read {
            Ok(read) => {
                let cleaned = filter::text::normalize(&read.lines);
                let chars: usize = cleaned.iter().map(|l| l.chars().count()).sum();
                let oordeel = if chars >= cfg.uia.min_text_len {
                    // TextPattern is de rijkste bron: hele documenten in één keer.
                    if read.document_text {
                        "uia (met documenttekst)"
                    } else {
                        "uia"
                    }
                } else if read.elements <= 1 {
                    "lege boom -> ocr"
                } else {
                    "te weinig tekst -> ocr"
                };
                (
                    read.elements.to_string(),
                    chars.to_string(),
                    read.document_ms.to_string(),
                    read.tree_ms.to_string(),
                    oordeel,
                )
            }
            Err(_) => ("-".into(), "-".into(), "-".into(), "-".into(), "geen element -> ocr"),
        };

        println!(
            "  {:<18} {elements:>7} {chars:>8} {doc_ms:>6} {tree_ms:>6}  {oordeel}",
            truncate(&window.app_key(), 18)
        );
    }
    Ok(())
}

/// Maakt één screenshot, draait OCR en rapporteert wat eruit kwam.
fn proefopname(cfg: &Config) -> Result<String> {
    let mut cap = capture::ScreenCapturer::new(&cfg.capture.monitor)?;
    let shots = cap.capture()?;
    let shot = shots.into_iter().next().ok_or_else(|| anyhow!("geen frame"))?;
    let (w, h) = shot.image.dimensions();

    let engine = ocr::windows_ocr::WindowsOcr::new(cfg.ocr.language.as_deref())
        .context("OCR-engine starten")?;
    let started = std::time::Instant::now();
    let raw = engine.recognize(&shot.image)?;
    let ms = started.elapsed().as_millis();

    let lines = filter::text::normalize(&raw.lines);
    let text = lines.join("\n");
    let score = filter::text::quality(&text);
    let chars = text.chars().count();

    let oordeel = if chars >= cfg.ocr.min_text_len && score >= cfg.ocr.min_quality {
        "wordt als tekst opgeslagen"
    } else {
        "zou terugvallen op beeld"
    };

    Ok(format!(
        "{w}x{h}, {} regels, {chars} tekens, kwaliteit {score:.2} in {ms} ms — {oordeel}",
        lines.len()
    ))
}

fn cmd_config(cfg: Config, path: &std::path::Path, init: bool) -> Result<()> {
    if init {
        if path.exists() {
            return Err(anyhow!(
                "{} bestaat al; verwijder of hernoem het eerst",
                path.display()
            ));
        }
        cfg.save(path)?;
        println!("Configuratie geschreven naar {}", path.display());
        return Ok(());
    }

    println!("# {}", path.display());
    if !path.exists() {
        println!("# (bestaat nog niet — dit zijn de defaults; `chronicle config --init` schrijft ze weg)");
    }
    println!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}

// --- kleine hulpjes -------------------------------------------------------

/// Parseert `30d`, `12h`, `45m`, `90s` of een kaal getal (dagen) naar seconden.
fn parse_duration(spec: &str) -> Result<i64> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(anyhow!("lege tijdsduur"));
    }
    let (digits, unit) = spec.split_at(
        spec.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(spec.len()),
    );
    let n: i64 = digits
        .parse()
        .with_context(|| format!("kan tijdsduur niet lezen: {spec}"))?;

    let secs = match unit.trim() {
        "" | "d" => n * 86_400,
        "h" => n * 3_600,
        "m" => n * 60,
        "s" => n,
        "w" => n * 604_800,
        other => return Err(anyhow!("onbekende eenheid {other:?}; gebruik s, m, h, d of w")),
    };
    Ok(secs)
}

fn fmt_ts(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format("%d-%m-%Y %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// Zet de `<<`/`>>`-markering van FTS5 om naar iets leesbaars.
fn markeer(s: &str, color: bool) -> String {
    if color {
        s.replace("<<", "\x1b[1;33m").replace(">>", "\x1b[0m")
    } else {
        s.replace("<<", "«").replace(">>", "»")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tijdsduur_parsen() {
        assert_eq!(parse_duration("30d").unwrap(), 30 * 86_400);
        assert_eq!(parse_duration("12h").unwrap(), 12 * 3_600);
        assert_eq!(parse_duration("45m").unwrap(), 45 * 60);
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("2w").unwrap(), 2 * 604_800);
        // Een kaal getal betekent dagen.
        assert_eq!(parse_duration("7").unwrap(), 7 * 86_400);
    }

    #[test]
    fn ongeldige_tijdsduur_geeft_een_fout() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("morgen").is_err());
        assert!(parse_duration("5j").is_err());
    }

    #[test]
    fn afkappen_telt_in_tekens_niet_bytes() {
        assert_eq!(truncate("hallo", 10), "hallo");
        assert_eq!(truncate("café-avond", 4), "café…");
    }
}
