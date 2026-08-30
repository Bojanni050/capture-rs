//! UI Automation als achtergronddienst, met een tijdslimiet en een geheugen.
//!
//! Twee dingen maken UIA lastig om zomaar in een opnamelus te hangen:
//!
//! 1. **Een aanroep kan blijven hangen.** UIA praat cross-process; staat de
//!    doelapp vast, dan staat de aanroep vast. Er is geen timeout-parameter,
//!    dus de lezer draait op een eigen thread en de pijplijn wacht met een
//!    deadline. Loopt die af, dan gaan we door met OCR en blijft de thread
//!    achter tot de app weer reageert — de volgende tik ziet dat de thread
//!    bezet is en slaat UIA meteen over.
//!
//! 2. **Niet elke app doet mee.** Games, oude Win32-programma's en sommige
//!    Electron-apps hebben een lege of nutteloze boom. Voor die apps is elke
//!    UIA-poging verspilling, dus we onthouden per app of het wat oplevert en
//!    zetten hem tijdelijk uit als het een paar keer niks werd.
//!
//! 3. **Event-driven modus.** Als `event_driven` ingeschakeld is, gebruiken we
//!    AddStructureChangedEventHandler en AddPropertyChangedEventHandler om de
//!    boom alleen uit te lezen wanneer de app zelf meldt dat er iets veranderd is.
//!    Dit is architectonisch beter: geen polling, alleen lezen bij veranderingen.

pub mod events;
pub mod reader;

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use windows::Win32::Foundation::HWND;

use crate::config::UiaConfig;
use events::{UiaEvent, UiaEventManager, start_event_thread};
use reader::{UiaReader, WindowRead};

struct Job {
    /// `HWND` is niet `Send`; het handvat reist als getal en wordt op de
    /// werkthread weer samengesteld.
    hwnd: isize,
    reply: oneshot::Sender<Result<WindowRead>>,
}

/// Wat een UIA-poging opleverde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Bruikbare tekst uit de accessibility-boom.
    Text(Vec<String>),
    /// Geen bruikbaar resultaat; de reden is bedoeld voor logs en statistiek.
    Unavailable(&'static str),
}

/// Wat we per app onthouden over de bruikbaarheid van UIA.
#[derive(Debug, Default)]
struct AppHealth {
    failures: u32,
    /// Zolang dit in de toekomst ligt, slaan we UIA voor deze app over.
    skip_until: Option<Instant>,
}

pub struct UiaService {
    tx: mpsc::Sender<Job>,
    pub(crate) cfg: UiaConfig,
    health: HashMap<String, AppHealth>,
    denylist: Vec<String>,
    /// Event manager voor event-driven UIA lezen.
    pub(crate) event_manager: Option<UiaEventManager>,
    /// Ontvanger voor UIA events.
    event_receiver: Option<std_mpsc::Receiver<UiaEvent>>,
    /// Thread handle voor de event processing thread.
    #[allow(dead_code)]
    event_thread: Option<std::thread::JoinHandle<()>>,
}

impl UiaService {
    /// Start de werkthread. Faalt als UI Automation niet te activeren is.
    pub fn start(cfg: UiaConfig) -> Result<Self> {
        // Diepte 1: zolang de vorige leesactie loopt, willen we geen tweede
        // opdracht in de wachtrij zetten maar meteen doorgaan naar OCR.
        let (tx, mut rx) = mpsc::channel::<Job>(1);
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<()>>();
        let max_elements = cfg.max_elements;

        std::thread::Builder::new()
            .name("chronicle-uia".into())
            .spawn(move || {
                let reader = match UiaReader::new(max_elements) {
                    Ok(reader) => {
                        let _ = init_tx.send(Ok(()));
                        reader
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                while let Some(job) = rx.blocking_recv() {
                    let hwnd = HWND(job.hwnd as *mut core::ffi::c_void);
                    let _ = job.reply.send(reader.read_window(hwnd));
                }
            })?;

        init_rx
            .recv()
            .map_err(|_| anyhow!("UIA-thread startte niet"))??;

        let denylist = cfg.app_denylist.iter().map(|a| a.to_lowercase()).collect();

        // Start event-driven UIA als dat ingeschakeld is
        let (event_manager, event_receiver, event_thread) = if cfg.event_driven {
            let (event_sender, event_receiver) = std_mpsc::channel();

            // Creer een UiaReader om de automation en cache te delen
            let reader = UiaReader::new(cfg.max_elements)?;

            let manager = UiaEventManager::new(
                reader.automation.clone(),
                reader.cache.clone(),
                event_sender.clone(),
            )?;
            let thread = start_event_thread(event_sender)?;
            (Some(manager), Some(event_receiver), Some(thread))
        } else {
            (None, None, None)
        };

        Ok(Self {
            tx,
            cfg,
            health: HashMap::new(),
            denylist,
            event_manager,
            event_receiver,
            event_thread,
        })
    }

    /// Probeert het venster te lezen. Geeft nooit een fout terug: elke
    /// tegenslag is gewoon een reden om naar OCR door te schuiven.
    pub async fn read(&mut self, app_key: &str, hwnd: isize) -> Outcome {
        if self.is_denied(app_key) {
            return Outcome::Unavailable("app op uia-denylist");
        }
        if self.is_sleeping(app_key) {
            return Outcome::Unavailable("uia levert niets voor deze app");
        }

        // Als event-driven UIA ingeschakeld is, check dan of er events zijn
        if let Some(ref event_receiver) = self.event_receiver {
            // Check of er een event is voor dit venster
            if let Ok(event) = event_receiver.try_recv() {
                match event {
                    UiaEvent::StructureChanged { hwnd: event_hwnd, .. } if event_hwnd == hwnd => {
                        // Er is een structurele verandering, lees de boom nu
                        return self.read_after_event(app_key, hwnd).await;
                    }
                    UiaEvent::PropertyChanged { hwnd: event_hwnd, .. } if event_hwnd == hwnd => {
                        // Er is een property verandering, lees de boom nu
                        tracing::debug!(app = app_key, "UIA property veranderd");
                        return self.read_after_event(app_key, hwnd).await;
                    }
                    _ => {
                        // Event is voor een ander venster, doe niets
                    }
                }
            }
        }

        // Normale leesactie als er geen event is
        let (reply, rx) = oneshot::channel();
        // Bezet? Dan hangt de vorige aanroep nog; niet wachten.
        if self.tx.try_send(Job { hwnd, reply }).is_err() {
            return Outcome::Unavailable("uia-thread bezet");
        }

        let deadline = Duration::from_millis(self.cfg.timeout_ms);
        let read = match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(Ok(read))) => read,
            Ok(Ok(Err(e))) => {
                tracing::debug!(app = app_key, error = %e, "uia-leesactie mislukt");
                self.record_failure(app_key);
                return Outcome::Unavailable("uia-fout");
            }
            Ok(Err(_)) => {
                self.record_failure(app_key);
                return Outcome::Unavailable("uia-thread gestopt");
            }
            Err(_) => {
                // De thread werkt nog door; de volgende tik ziet hem als bezet.
                tracing::debug!(app = app_key, "uia-timeout");
                self.record_failure(app_key);
                return Outcome::Unavailable("uia-timeout");
            }
        };

        if read.chars() < self.cfg.min_text_len {
            self.record_failure(app_key);
            return Outcome::Unavailable("uia gaf te weinig tekst");
        }

        self.record_success(app_key);
        Outcome::Text(read.lines)
    }

    /// Lees de boom na een event (event-driven modus).
    async fn read_after_event(&mut self, app_key: &str, hwnd: isize) -> Outcome {
        if let Some(ref manager) = self.event_manager {
            let hwnd_obj = HWND(hwnd as *mut core::ffi::c_void);

            // Lees direct de boom na het event
            match manager.read_window_after_event(hwnd_obj) {
                Ok(read) => {
                    if read.chars() >= self.cfg.min_text_len {
                        self.record_success(app_key);
                        return Outcome::Text(read.lines);
                    } else {
                        self.record_failure(app_key);
                        return Outcome::Unavailable("uia gaf te weinig tekst na event");
                    }
                }
                Err(e) => {
                    tracing::debug!(app = app_key, error = %e, "uia-lezen na event mislukt");
                    self.record_failure(app_key);
                    return Outcome::Unavailable("uia-fout na event");
                }
            }
        }

        // Als event-driven niet beschikbaar is, gebruik de normale methode
        Outcome::Unavailable("event-driven uia niet beschikbaar")
    }

    fn is_denied(&self, app_key: &str) -> bool {
        let key = app_key.to_lowercase();
        self.denylist.iter().any(|a| key.contains(a.as_str()))
    }

    /// Staat deze app in de afkoelperiode na herhaalde teleurstellingen?
    fn is_sleeping(&self, app_key: &str) -> bool {
        self.health
            .get(app_key)
            .and_then(|h| h.skip_until)
            .is_some_and(|until| Instant::now() < until)
    }

    fn record_failure(&mut self, app_key: &str) {
        let health = self.health.entry(app_key.to_string()).or_default();
        health.failures = health.failures.saturating_add(1);

        if health.failures >= self.cfg.failures_before_skip {
            health.skip_until =
                Some(Instant::now() + Duration::from_secs(self.cfg.retry_after_secs));
            // Een strafpunt terug, zodat de app na de afkoelperiode nog een
            // eerlijke kans krijgt voordat hij er weer uit vliegt. Apps
            // schakelen accessibility soms alsnog in.
            health.failures = self.cfg.failures_before_skip.saturating_sub(1);
            tracing::debug!(
                app = app_key,
                seconden = self.cfg.retry_after_secs,
                "uia tijdelijk uitgezet voor deze app"
            );
        }
    }

    fn record_success(&mut self, app_key: &str) {
        if let Some(health) = self.health.get_mut(app_key) {
            health.failures = 0;
            health.skip_until = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiaConfig;

    /// Bouwt een dienst zonder werkthread, zodat de boekhouding los te testen is.
    fn service(cfg: UiaConfig) -> UiaService {
        let (tx, _rx) = mpsc::channel(1);
        let denylist = cfg.app_denylist.iter().map(|a| a.to_lowercase()).collect();
        UiaService {
            tx,
            cfg,
            health: HashMap::new(),
            denylist,
            event_manager: None,
            event_receiver: None,
            event_thread: None,
        }
    }

    #[test]
    fn denylist_matcht_ongeacht_hoofdletters() {
        let s = service(UiaConfig {
            app_denylist: vec!["Photoshop".into()],
            ..Default::default()
        });
        assert!(s.is_denied("photoshop"));
        assert!(!s.is_denied("notepad"));
    }

    #[test]
    fn app_gaat_slapen_na_genoeg_mislukkingen() {
        let mut s = service(UiaConfig {
            failures_before_skip: 3,
            retry_after_secs: 600,
            ..Default::default()
        });

        assert!(!s.is_sleeping("game"));
        s.record_failure("game");
        s.record_failure("game");
        assert!(!s.is_sleeping("game"), "twee keer is nog geen patroon");
        s.record_failure("game");
        assert!(s.is_sleeping("game"), "derde mislukking moet hem uitzetten");
    }

    #[test]
    fn succes_wist_het_strafblad() {
        let mut s = service(UiaConfig {
            failures_before_skip: 2,
            ..Default::default()
        });

        s.record_failure("editor");
        s.record_failure("editor");
        assert!(s.is_sleeping("editor"));

        s.record_success("editor");
        assert!(!s.is_sleeping("editor"));
    }

    #[test]
    fn na_afkoelen_krijgt_een_app_nog_een_kans() {
        let mut s = service(UiaConfig {
            failures_before_skip: 2,
            retry_after_secs: 0, // meteen weer wakker
            ..Default::default()
        });

        s.record_failure("app");
        s.record_failure("app");
        assert!(!s.is_sleeping("app"), "afkoelperiode van 0 is meteen voorbij");

        // Het strafblad staat op een, dus een mislukking zet hem weer uit.
        s.record_failure("app");
        assert_eq!(s.health["app"].failures, 1);
    }
}
