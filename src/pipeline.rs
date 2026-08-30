//! De opnamelus: kijken, filteren, lezen, opslaan.
//!
//! Per tik doorloopt een frame een reeks beslissingen, van goedkoop naar duur:
//!
//! ```text
//!   venster + idle ──poort──▶ screenshot ──beeldhash──▶ UIA ──▶ OCR ──▶ opslag
//!         │                        │                     │       │        │
//!     overslaan               onveranderd            te weinig  te     tekst of
//!                             → segment rekken        tekst   rommelig  beeld
//! ```
//!
//! Drie tekstbronnen, in volgorde van betrouwbaarheid:
//!
//! 1. **UI Automation** — de tekens die de app zelf aan schermlezers geeft.
//!    Exact en goedkoop, maar niet elke app doet mee.
//! 2. **OCR** — pixels lezen. Werkt overal, maar raadt soms verkeerd, dus de
//!    uitkomst moet door een kwaliteitsdrempel.
//! 3. **Beeld** — geeft geen van beide iets bruikbaars (video, spel, foto,
//!    ontwerptool), dan bewaren we het frame zelf, zodat er nooit een gat in
//!    je tijdlijn valt.

use crate::capture::{ForegroundEvents, ScreenCapturer, WindowInfo, foreground, idle_seconds};
use crate::config::{Config, FramePolicy};
use crate::filter::{dedupe, Gate, NoiseFilter};
use crate::ocr::OcrService;
use crate::store::{Db, FrameStore, NewCapture};
use crate::uia::{self, UiaService};
use anyhow::{Context, Result};
use chrono::Local;
use image::RgbaImage;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::block_in_place;

/// Hoe vaak we het geleerde boilerplate-geheugen wegschrijven (in tikken).
const FLUSH_EVERY: u64 = 50;
/// Hoe vaak de retentie-opruiming draait.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(3_600);
/// Geeft een nieuw voorgrondvenster even de tijd om zijn titel/UIA-boom bij te
/// werken en voorkomt captures tijdens een snelle Alt-Tab-reeks.
const FOREGROUND_DEBOUNCE: Duration = Duration::from_millis(500);

struct Segment {
    id: i64,
    app_key: String,
    title: String,
}

/// Welke laag de tekst uiteindelijk leverde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Uia,
    Ocr,
    None,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Uia => "uia",
            Source::Ocr => "ocr",
            Source::None => "none",
        }
    }
}

/// Alles wat één frame nodig heeft behalve het beeld zelf.
struct FrameContext<'a> {
    segment_id: i64,
    app_key: &'a str,
    hwnd: isize,
    monitor: &'a str,
    ts: i64,
    now: &'a chrono::DateTime<Local>,
}

/// Uitkomst van een leespoging over alle tekstbronnen heen.
struct TextRead {
    lines: Vec<String>,
    source: Source,
    /// Waarom een eerdere bron afviel; belandt in `fallback_reason`.
    note: Option<String>,
}

pub struct Pipeline {
    cfg: Config,
    db: Arc<Db>,
    frames: Arc<FrameStore>,
    uia: Option<UiaService>,
    ocr: Option<OcrService>,
    filter: NoiseFilter,
    capturer: ScreenCapturer,
    segment: Option<Segment>,
    app_ids: HashMap<String, i64>,
    ticks: u64,
}

impl Pipeline {
    pub fn new(cfg: Config, db: Arc<Db>, frames: Arc<FrameStore>) -> Result<Self> {
        let capturer = ScreenCapturer::new(&cfg.capture.monitor)?;

        let uia = if cfg.uia.enabled {
            match UiaService::start(cfg.uia.clone()) {
                Ok(service) => {
                    tracing::info!("UI Automation actief als primaire tekstbron");
                    Some(service)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "UI Automation niet beschikbaar, OCR doet het werk");
                    None
                }
            }
        } else {
            None
        };

        let ocr = if cfg.ocr.enabled {
            match OcrService::start(cfg.ocr.language.clone()) {
                Ok(service) => {
                    tracing::info!(taal = service.language(), "OCR actief");
                    Some(service)
                }
                Err(e) => {
                    // Geen OCR is vervelend maar niet fataal: alles valt dan
                    // terug op beeld, en dat is precies waar de fallback voor is.
                    tracing::warn!(error = %e, "OCR niet beschikbaar, alles wordt als beeld bewaard");
                    None
                }
            }
        } else {
            None
        };

        let mut filter = NoiseFilter::new(cfg.filter.clone())?;
        for (app, frames_seen, lines) in db.load_boilerplate()? {
            filter.restore(&app, frames_seen, lines);
        }

        Ok(Self {
            cfg,
            db,
            frames,
            uia,
            ocr,
            filter,
            capturer,
            segment: None,
            app_ids: HashMap::new(),
            ticks: 0,
        })
    }

    pub fn monitors(&self) -> String {
        self.capturer.describe()
    }

    /// Draait tot `shutdown` afgaat.
    pub async fn run(&mut self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let mut maintenance = tokio::time::interval(MAINTENANCE_INTERVAL);
        maintenance.tick().await; // de eerste tik komt meteen; die slaan we over
        let (foreground_events, mut foreground_changes) = match ForegroundEvents::start() {
            Ok(events) => {
                tracing::info!("voorgrondvenster-events actief");
                (Some(events.0), events.1)
            }
            Err(e) => {
                // De periodieke route blijft volledig bruikbaar wanneer een
                // sandbox of oude Windows-versie geen hook toestaat.
                tracing::warn!(error = %e, "voorgrondvenster-events niet beschikbaar; alleen interval actief");
                let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
                // Houd geen hook vast; de receiver sluit nooit en de
                // fallback-timer blijft de opname aansturen.
                (None, rx)
            }
        };

        loop {
            let idle = idle_seconds();
            let sleeping = idle >= self.cfg.capture.idle_after_secs;

            if let Err(e) = self.tick(idle).await {
                tracing::warn!(error = %e, "tik overgeslagen");
            }

            let interval = if sleeping {
                self.cfg.capture.idle_interval_secs
            } else {
                self.cfg.capture.interval_secs
            };
            let interval = Duration::from_secs_f64(interval.max(0.5));

            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = maintenance.tick() => {
                    if let Err(e) = self.maintenance().await {
                        tracing::warn!(error = %e, "onderhoud mislukt");
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
                Some(_) = foreground_changes.recv() => {
                    // Windows kan bij één overgang meerdere meldingen geven.
                    // De inhoud en titel hebben bovendien een kort moment
                    // nodig om stabiel te worden.
                    tokio::time::sleep(FOREGROUND_DEBOUNCE).await;
                    while foreground_changes.try_recv().is_ok() {}
                }
            }
        }

        drop(foreground_events);
        self.flush_boilerplate()?;
        tracing::info!("opname gestopt");
        Ok(())
    }

    async fn tick(&mut self, idle: u64) -> Result<()> {
        self.ticks += 1;
        let now = Local::now();
        let ts = now.timestamp();

        let window = foreground();

        // Laag 1: de poort.
        if let Gate::Skip(reason) =
            self.filter
                .gate(window.as_ref(), idle, self.cfg.capture.idle_after_secs)
        {
            self.db
                .record_skip(&now.format("%Y-%m-%d").to_string(), reason.as_str())?;
            // Een lopend segment eindigt zodra je iets anders gaat doen.
            self.segment = None;
            return Ok(());
        }
        let window = window.expect("gate laat None niet door");

        let app_key = window.app_key();
        let app_id = self.app_id(&window)?;
        let segment_id = self.segment_for(app_id, &app_key, &window.title, ts)?;

        // Laag 2 moet vóór een tekstbron lopen: een onveranderd scherm hoeft
        // noch UIA noch OCR. Bij meerdere schermen gebruiken we OCR per
        // scherm; UIA hoort bij één voorgrondvenster en is geen bron voor de
        // inhoud van een tweede monitor.
        tracing::trace!("screenshot maken");
        let shots = block_in_place(|| self.capturer.capture())?;
        tracing::trace!(frames = shots.len(), "screenshot klaar");

        // Als event-driven UIA ingeschakeld is, registreer dan event handlers
        // voor het voorgrondvenster
        if let Some(ref mut uia_service) = self.uia {
            if uia_service.cfg.event_driven {
                if let Some(ref manager) = uia_service.event_manager {
                    let hwnd_obj = HWND(window.hwnd as *mut core::ffi::c_void);
                    if let Err(e) = manager.register_window_events(hwnd_obj) {
                        tracing::debug!(app = app_key, error = %e, "UIA event registratie mislukt");
                    }
                }
            }
        }

        for shot in shots {
            self.process_frame(
                FrameContext {
                    segment_id,
                    app_key: &app_key,
                    hwnd: window.hwnd,
                    monitor: &shot.monitor,
                    ts,
                    now: &now,
                },
                shot.image,
            )
            .await?;
        }

        if self.ticks.is_multiple_of(FLUSH_EVERY) {
            self.flush_boilerplate()?;
        }
        Ok(())
    }

    async fn process_frame(
        &mut self,
        ctx: FrameContext<'_>,
        image: RgbaImage,
    ) -> Result<()> {
        let FrameContext {
            segment_id,
            app_key,
            hwnd,
            monitor,
            ts,
            now,
        } = ctx;
        let day = now.format("%Y-%m-%d").to_string();

        // Laag 2: is er iets veranderd op dit scherm?
        let hash = block_in_place(|| dedupe::dhash(&image));
        let scope = format!("{app_key}@{monitor}");
        if self.filter.frame_is_duplicate(&scope, hash) {
            self.db.touch_segment(segment_id, ts)?;
            self.db.record_skip(&day, "onveranderd beeld")?;
            return Ok(());
        }

        // Laag 3: lezen wat er staat — eerst de accessibility-boom, dan OCR.
        // Bij meerdere monitoren wordt UIA bewust niet op een willekeurig
        // scherm toegepast: het beschrijft alleen het voorgrondvenster.
        let single_monitor = self.capturer.is_single_monitor();
        let read = if single_monitor {
            self.read_text(app_key, hwnd, &image).await
        } else {
            self.read_ocr(&image, None).await
        };
        // De teksttoestand is per scherm wanneer we alle monitoren opnemen.
        // Anders zou gelijke tekst op monitor 1 monitor 2 als herhaling
        // wegdrukken voordat die tweede capture opgeslagen wordt.
        let text_scope = if single_monitor { app_key } else { &scope };
        let analysis = self.filter.analyze_text(text_scope, &read.lines);

        // Zelfde woorden als het vorige frame: er bewoog iets (een cursor, een
        // video in een hoek), maar inhoudelijk is er niets nieuws.
        if analysis.same_as_previous {
            self.db.touch_segment(segment_id, ts)?;
            self.db.record_skip(&day, "zelfde tekst")?;
            return Ok(());
        }

        // Laag 4: vertrouwen we deze tekst, of vallen we terug op het beeld?
        let char_len = analysis.char_len();
        let fallback_reason = match read.source {
            // UIA levert de échte tekens, dus daar hoeft geen kwaliteitsoordeel
            // overheen; alleen de vraag of er ná filtering nog inhoud over is.
            Source::Uia if char_len < self.cfg.ocr.min_text_len => Some(format!(
                "uia-tekst bleef niet over na filtering ({char_len} tekens)"
            )),
            Source::Uia => None,

            Source::Ocr if char_len < self.cfg.ocr.min_text_len => {
                Some(format!("te weinig tekst ({char_len} tekens)"))
            }
            Source::Ocr if analysis.quality < self.cfg.ocr.min_quality => Some(format!(
                "lage tekstkwaliteit ({:.2} < {:.2})",
                analysis.quality, self.cfg.ocr.min_quality
            )),
            Source::Ocr => None,

            Source::None => Some(read.note.clone().unwrap_or_else(|| "geen tekstbron".into())),
        };

        let kind = if fallback_reason.is_some() {
            "image"
        } else {
            "text"
        };
        // De opgeslagen bron is die welke de bewaarde tekst leverde; valt alles
        // terug op beeld, dan is er geen tekstbron en zegt de reden waarom.
        let source = if kind == "image" {
            Source::None
        } else {
            read.source
        };

        let keep_frame = match self.cfg.storage.keep_frames {
            FramePolicy::Always => true,
            FramePolicy::Fallback => kind == "image",
            FramePolicy::Never => false,
        };

        let frame = if keep_frame {
            let frames = Arc::clone(&self.frames);
            match block_in_place(|| frames.save(&image, ts)) {
                Ok(saved) => Some(saved),
                Err(e) => {
                    tracing::warn!(error = %e, "frame opslaan mislukt");
                    None
                }
            }
        } else {
            None
        };

        // Zonder tekst én zonder beeld is er niets om te bewaren; het segment
        // legt dan nog steeds vast dát je hier was.
        if kind == "image" && frame.is_none() {
            self.db.touch_segment(segment_id, ts)?;
            self.db.record_skip(&day, "geen bruikbare inhoud")?;
            return Ok(());
        }

        let (frame_path, width, height) = match &frame {
            Some((path, w, h)) => (Some(path.as_str()), *w, *h),
            None => (None, image.width(), image.height()),
        };

        self.db.insert_capture(NewCapture {
            segment_id,
            ts,
            kind,
            source: source.as_str(),
            monitor,
            phash: hash,
            quality: analysis.quality,
            text: &analysis.full_text,
            index_text: &analysis.index_text,
            frame_path,
            width,
            height,
            fallback_reason: fallback_reason.as_deref(),
        })?;

        tracing::debug!(
            app = app_key,
            soort = kind,
            bron = source.as_str(),
            tekens = char_len,
            kwaliteit = analysis.quality,
            boilerplate = analysis.boilerplate_lines,
            "vastgelegd"
        );
        Ok(())
    }

    /// Haalt tekst op via UIA en valt bij een onbruikbare boom terug op OCR.
    async fn read_text(&mut self, app_key: &str, hwnd: isize, image: &RgbaImage) -> TextRead {
        let mut note = None;

        if let Some(service) = &mut self.uia {
            match service.read(app_key, hwnd).await {
                uia::Outcome::Text(lines) => {
                    return TextRead {
                        lines,
                        source: Source::Uia,
                        note: None,
                    };
                }
                uia::Outcome::Unavailable(reason) => {
                    tracing::trace!(app = app_key, reden = reason, "uia overgeslagen");
                    note = Some(reason.to_string());
                }
            }
        }

        self.read_ocr(image, note).await
    }

    /// Leest één screenshot via OCR, met optioneel de reden waarom UIA afviel.
    async fn read_ocr(&self, image: &RgbaImage, note: Option<String>) -> TextRead {
        if let Some(service) = &self.ocr {
            match service.recognize(image.clone()).await {
                Ok(raw) => {
                    return TextRead {
                        lines: raw.lines,
                        source: Source::Ocr,
                        note,
                    };
                }
                Err(e) => {
                    tracing::debug!(error = %e, "OCR mislukt");
                    return TextRead {
                        lines: Vec::new(),
                        source: Source::None,
                        note: Some(format!("OCR mislukt: {e}")),
                    };
                }
            }
        }

        TextRead {
            lines: Vec::new(),
            source: Source::None,
            note: Some(note.unwrap_or_else(|| "geen tekstbron actief".into())),
        }
    }

    fn app_id(&mut self, window: &WindowInfo) -> Result<i64> {
        let key = window.app_key();
        if let Some(id) = self.app_ids.get(&key) {
            return Ok(*id);
        }
        let id = self.db.app_id(&key, &window.exe, &window.exe_path)?;
        self.app_ids.insert(key, id);
        Ok(id)
    }

    /// Geeft het huidige segment, of opent een nieuw als je van app of venster
    /// bent gewisseld.
    fn segment_for(&mut self, app_id: i64, app_key: &str, title: &str, ts: i64) -> Result<i64> {
        if let Some(seg) = &self.segment
            && seg.app_key == app_key
            && seg.title == title
        {
            return Ok(seg.id);
        }
        let id = self.db.open_segment(app_id, title, ts)?;
        self.segment = Some(Segment {
            id,
            app_key: app_key.to_string(),
            title: title.to_string(),
        });
        Ok(id)
    }

    /// Schrijft geleerde boilerplate weg zodat het filter een herstart overleeft.
    fn flush_boilerplate(&mut self) -> Result<()> {
        for (app_key, frames, lines) in self.filter.drain_dirty() {
            let Some(app_id) = self.app_ids.get(&app_key).copied() else {
                continue;
            };
            self.db.save_boilerplate(app_id, frames, &lines)?;
        }
        Ok(())
    }

    async fn maintenance(&mut self) -> Result<()> {
        self.flush_boilerplate()?;
        let report = run_retention(&self.cfg, &self.db, &self.frames)?;
        if report > 0 {
            tracing::info!(opgeruimd = report, "retentie toegepast");
        }
        Ok(())
    }
}

/// Past het retentiebeleid toe. Geeft terug hoeveel items zijn opgeruimd.
pub fn run_retention(cfg: &Config, db: &Db, frames: &FrameStore) -> Result<i64> {
    let now = Local::now().timestamp();
    let day = 86_400i64;

    let captures_before = match cfg.storage.retention_days {
        0 => None,
        d => Some(now - d as i64 * day),
    };
    let frames_before = match cfg.storage.frame_retention_days {
        0 => None,
        d => Some(now - d as i64 * day),
    };

    let report = db
        .purge(captures_before, frames_before)
        .context("opruimen mislukt")?;

    for path in &report.frame_paths {
        if let Err(e) = frames.delete(path) {
            tracing::debug!(error = %e, pad = path, "frame verwijderen mislukt");
        }
    }
    if !report.frame_paths.is_empty() {
        frames.prune_empty_dirs();
    }

    Ok(report.captures_deleted + report.frames_deleted)
}
