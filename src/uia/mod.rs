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
//! 3. **Events als signaal, niet als kortere weg.** Als `event_driven`
//!    ingeschakeld is, registreren we AddStructureChangedEventHandler en
//!    AddPropertyChangedEventHandler op het voorgrondvenster. Een event zegt
//!    alleen dát er iets veranderd is; de tik-lus blijft de screenshots en de
//!    leescadans bepalen, en elke lezing — event of niet — gaat via dezelfde
//!    `read_via_worker`: toegewijde thread, altijd met een deadline. Een
//!    eerdere versie liet de event-tak rechtstreeks en zonder deadline lezen;
//!    dat was precies het gevaar waar punt 1 hierboven voor waarschuwt.

pub mod events;
pub mod reader;

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};
use windows::Win32::Foundation::HWND;

use crate::capture::Rect;
use crate::config::UiaConfig;
use events::UiaEventThread;
use reader::{UiaReader, WindowRead};

struct Job {
    /// `HWND` is niet `Send`; het handvat reist als getal en wordt op de
    /// werkthread weer samengesteld.
    hwnd: isize,
    /// Alleen voor browservensters: vraag de favorietenbalk vroeg op, vóór de
    /// dure boomwandeling. Zo hebben we de rechthoek ook als de lezing zelf
    /// in een timeout loopt.
    bar_reply: Option<oneshot::Sender<Option<Rect>>>,
    /// `None` bij een geometrie-only opdracht: dan hoeft de dure
    /// boomwandeling niet te draaien.
    reply: Option<oneshot::Sender<Result<WindowRead>>>,
}

/// Wat een UIA-poging opleverde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Bruikbare tekst uit de accessibility-boom.
    Text(Vec<String>),
    /// Geen bruikbaar resultaat; de reden is bedoeld voor logs en statistiek.
    /// De favorietenbalk gaat mee als die vóór de mislukte lezing al gevonden
    /// was, zodat OCR hem alsnog kan maskeren.
    Unavailable {
        reason: &'static str,
        bookmark_bar: Option<Rect>,
    },
    /// Het venster bevat een wachtwoordveld. Geen tekst, geen OCR: de
    /// pipeline sluit het venster (of domein) uit.
    PasswordField,
}

/// Wat we per app onthouden over de bruikbaarheid van UIA.
#[derive(Debug, Default)]
struct AppHealth {
    failures: u32,
    /// Zolang dit in de toekomst ligt, slaan we UIA voor deze app over.
    skip_until: Option<Instant>,
    /// Waarom de laatste poging afviel. Een app die alleen te weinig tekst
    /// gaf is nog wel bereikbaar; een app die timede is dat niet.
    last_failure: Option<&'static str>,
}

pub struct UiaService {
    tx: mpsc::Sender<Job>,
    pub(crate) cfg: UiaConfig,
    health: HashMap<String, AppHealth>,
    denylist: Vec<String>,
    /// STA-thread die UIA-events registreert en ontvangt. Sluit zichzelf
    /// netjes af zodra dit veld valt (`UiaEventThread::drop`).
    pub(crate) event_manager: Option<UiaEventThread>,
    /// Versieteller voor UIA-events. `watch` coalescet bursts tot één
    /// "er is iets gewijzigd"-signaal, dus een druk document kan geen
    /// onbeperkte wachtrij opbouwen.
    event_signal: Option<watch::Sender<u64>>,
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
            .name("capture-uia".into())
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
                    if let Some(reply) = job.bar_reply {
                        let _ = reply.send(reader.bookmark_bar(hwnd));
                    }
                    if let Some(reply) = job.reply {
                        let _ = reply.send(reader.read_window(hwnd));
                    }
                }
            })?;

        init_rx
            .recv()
            .map_err(|_| anyhow!("UIA-thread startte niet"))??;

        let denylist = cfg
            .app_denylist
            .iter()
            .map(|a| crate::filter::privacy::normalize_app_entry(a))
            .filter(|a| !a.is_empty())
            .collect();

        // Start event-driven UIA als dat ingeschakeld is. De STA-thread bouwt
        // zijn eigen IUIAutomation-instantie op — COM-objecten zijn
        // apartment-gebonden en kunnen niet gedeeld worden met de MTA-thread
        // hierboven, ook niet via clone().
        let (event_manager, event_signal) = if cfg.event_driven {
            let (event_signal, _initial_receiver) = watch::channel(0u64);
            match UiaEventThread::start(event_signal.clone()) {
                Ok(thread) => (Some(thread), Some(event_signal)),
                Err(e) => {
                    tracing::warn!(error = %e, "uia-eventthread niet beschikbaar; alleen polling");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        Ok(Self {
            tx,
            cfg,
            health: HashMap::new(),
            denylist,
            event_manager,
            event_signal,
        })
    }

    /// Probeert het venster te lezen. Geeft nooit een fout terug: elke
    /// tegenslag is gewoon een reden om naar OCR door te schuiven.
    pub async fn read(
        &mut self,
        app_key: &str,
        hwnd: isize,
        want_bookmark_bar: bool,
    ) -> Outcome {
        if self.is_denied(app_key) {
            return Outcome::Unavailable {
                reason: "app op uia-denylist",
                bookmark_bar: None,
            };
        }
        if self.is_sleeping(app_key) {
            // Een slapende app krijgt geen dure boomwandeling, maar de
            // favorietenbalk is een gerichte query van enkele tientallen
            // milliseconden. Alleen doen als de app bereikbaar bleek: bij
            // "te weinig tekst" antwoordde de provider tenminste, bij een
            // timeout of fout laten we hem met rust.
            let reachable = self
                .health
                .get(app_key)
                .and_then(|h| h.last_failure)
                == Some("uia gaf te weinig tekst");
            let bookmark_bar = if want_bookmark_bar && reachable {
                self.query_bar_only(hwnd).await
            } else {
                None
            };
            return Outcome::Unavailable {
                reason: "uia levert niets voor deze app",
                bookmark_bar,
            };
        }

        // Goedkope controle, elke tik: zit de eventthread vast in een
        // registratie bij een provider zonder deadline? Verandert niets aan
        // de leesactie hieronder — die loopt sowieso altijd via de
        // toegewijde, beveiligde werkthread — maar zonder dit zou een
        // vastgelopen eventthread onzichtbaar zijn.
        if let Some(manager) = &self.event_manager {
            manager.warn_if_stuck(app_key);
        }

        self.read_via_worker(app_key, hwnd, want_bookmark_bar).await
    }

    /// Meldt de event-registratie af voor het huidige venster. Voor een app op
    /// de denylist mag Capture geen enkele UIA-aanroep meer doen — ook de
    /// event-registratie niet, want ook die wordt door de provider zelf
    /// bediend en kan een trage app verder belasten.
    pub fn detach_events(&mut self) {
        if let Some(manager) = &self.event_manager {
            manager.detach();
        }
    }

    /// Wisselt de event-registratie naar een nieuw venster, mits die app niet op
    /// de uia-denylist staat. De leesroute (`read`) controleerde de denylist
    /// al; deze aanroep deed dat niet, waardoor een geweigerde app alsnog een
    /// event-registratie opliep bij elke voorgrondwissel.
    pub fn switch_events(&mut self, app_key: &str, hwnd: isize) {
        if self.is_denied(app_key) {
            self.detach_events();
            return;
        }
        if let Some(manager) = &self.event_manager {
            let hwnd_obj = HWND(hwnd as *mut core::ffi::c_void);
            if let Err(e) = manager.switch_window(hwnd_obj) {
                tracing::debug!(app = app_key, error = %e, "UIA event registratie mislukt");
            }
        }
    }

    /// Geeft een ontvanger die bij elke UIA-wijziging wakker wordt. De waarde
    /// zelf is alleen een versieteller; de pipeline leest daarna via de
    /// bestaande worker met deadline en behandelt het event nooit als data.
    pub fn subscribe_events(&self) -> Option<watch::Receiver<u64>> {
        self.event_signal.as_ref().map(watch::Sender::subscribe)
    }

    /// Vraagt alleen de favorietenbalk op, zonder de boomwandeling. Bedoeld
    /// voor een app die in de afkoelperiode zit: de provider antwoordde daar
    /// eerder wel, dus een korte gerichte query is verantwoord.
    async fn query_bar_only(&mut self, hwnd: isize) -> Option<Rect> {
        let (bar_tx, bar_rx) = oneshot::channel();
        if self
            .tx
            .try_send(Job {
                hwnd,
                bar_reply: Some(bar_tx),
                reply: None,
            })
            .is_err()
        {
            return None;
        }

        let deadline = Duration::from_millis(self.cfg.timeout_ms.min(500));
        tokio::time::timeout(deadline, bar_rx).await.ok()?.ok()?
    }

    /// De enige plek waar UIA daadwerkelijk gelezen wordt: altijd via de
    /// toegewijde werkthread, altijd met een deadline. Zowel de normale
    /// route als de event-route komen hier samen, zodat er geen tweede,
    /// onbeveiligd leespad kan ontstaan.
    async fn read_via_worker(
        &mut self,
        app_key: &str,
        hwnd: isize,
        want_bookmark_bar: bool,
    ) -> Outcome {
        let (reply, rx) = oneshot::channel();
        let (bar_tx, mut bar_rx) = oneshot::channel();
        // Bezet? Dan hangt de vorige aanroep nog; niet wachten.
        if self
            .tx
            .try_send(Job {
                hwnd,
                bar_reply: want_bookmark_bar.then_some(bar_tx),
                reply: Some(reply),
            })
            .is_err()
        {
            return Outcome::Unavailable {
                reason: "uia-thread bezet",
                bookmark_bar: None,
            };
        }

        let deadline = Duration::from_millis(self.cfg.timeout_ms);
        let result = tokio::time::timeout(deadline, rx).await;
        // De werkthread stuurt de balk vóór de boomwandeling. Bij een timeout
        // staat de rechthoek er dus al, tenzij de bar-query zelf vastloopt.
        let bookmark_bar = bar_rx.try_recv().ok().flatten();

        let read = match result {
            Ok(Ok(Ok(read))) => read,
            Ok(Ok(Err(e))) => {
                tracing::debug!(app = app_key, error = %e, "uia-leesactie mislukt");
                self.record_failure(app_key, "uia-fout");
                return Outcome::Unavailable {
                    reason: "uia-fout",
                    bookmark_bar,
                };
            }
            Ok(Err(_)) => {
                self.record_failure(app_key, "uia-thread gestopt");
                return Outcome::Unavailable {
                    reason: "uia-thread gestopt",
                    bookmark_bar,
                };
            }
            Err(_) => {
                // De thread werkt nog door; de volgende tik ziet hem als bezet.
                tracing::debug!(app = app_key, "uia-timeout");
                self.record_failure(app_key, "uia-timeout");
                return Outcome::Unavailable {
                    reason: "uia-timeout",
                    bookmark_bar,
                };
            }
        };

        if read.password_field {
            return Outcome::PasswordField;
        }

        if read.chars() < self.cfg.min_text_len {
            self.record_failure(app_key, "uia gaf te weinig tekst");
            return Outcome::Unavailable {
                reason: "uia gaf te weinig tekst",
                bookmark_bar,
            };
        }

        self.record_success(app_key);
        Outcome::Text(read.lines)
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

    fn record_failure(&mut self, app_key: &str, reason: &'static str) {
        let health = self.health.entry(app_key.to_string()).or_default();
        health.failures = health.failures.saturating_add(1);
        health.last_failure = Some(reason);

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
            health.last_failure = None;
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
        let denylist = cfg
            .app_denylist
            .iter()
            .map(|a| crate::filter::privacy::normalize_app_entry(a))
            .collect();
        UiaService {
            tx,
            cfg,
            health: HashMap::new(),
            denylist,
            event_manager: None,
            event_signal: None,
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
    fn everything_staat_standaard_op_de_uia_denylist() {
        // De default-denylist bestaat om precies dit probleem: de
        // gevirtualiseerde lijst van Everything bevriest onder de
        // subtree-sweep.
        assert!(UiaConfig::default()
            .app_denylist
            .iter()
            .any(|a| a.eq_ignore_ascii_case("everything")));
    }

    #[test]
    fn app_gaat_slapen_na_genoeg_mislukkingen() {
        let mut s = service(UiaConfig {
            failures_before_skip: 3,
            retry_after_secs: 600,
            ..Default::default()
        });

        assert!(!s.is_sleeping("game"));
        s.record_failure("game", "test");
        s.record_failure("game", "test");
        assert!(!s.is_sleeping("game"), "twee keer is nog geen patroon");
        s.record_failure("game", "test");
        assert!(s.is_sleeping("game"), "derde mislukking moet hem uitzetten");
    }

    #[test]
    fn succes_wist_het_strafblad() {
        let mut s = service(UiaConfig {
            failures_before_skip: 2,
            ..Default::default()
        });

        s.record_failure("editor", "test");
        s.record_failure("editor", "test");
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

        s.record_failure("app", "test");
        s.record_failure("app", "test");
        assert!(!s.is_sleeping("app"), "afkoelperiode van 0 is meteen voorbij");

        // Het strafblad staat op een, dus een mislukking zet hem weer uit.
        s.record_failure("app", "test");
        assert_eq!(s.health["app"].failures, 1);
    }
}
