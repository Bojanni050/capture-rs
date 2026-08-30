//! Schermafbeeldingen ophalen via `xcap`.
//!
//! De monitorlijst wordt gecachet: `Monitor::all()` doet een enumeratie die
//! we niet elke vier seconden hoeven te herhalen. Bij een fout (monitor
//! losgekoppeld, resolutie gewijzigd) verversen we de cache en proberen we
//! het één keer opnieuw.

use anyhow::{anyhow, Context, Result};
use image::RgbaImage;
use xcap::Monitor;

pub struct ScreenCapturer {
    selector: MonitorSelector,
    monitors: Vec<Monitor>,
}

#[derive(Debug, Clone, Copy)]
enum MonitorSelector {
    Primary,
    All,
    Index(usize),
}

pub struct Shot {
    pub image: RgbaImage,
    pub monitor: String,
}

impl ScreenCapturer {
    /// `spec` is "primary", "all" of een index als tekst ("0", "1", ...).
    pub fn new(spec: &str) -> Result<Self> {
        let selector = match spec.trim().to_lowercase().as_str() {
            "primary" | "" => MonitorSelector::Primary,
            "all" => MonitorSelector::All,
            other => MonitorSelector::Index(
                other
                    .parse()
                    .with_context(|| format!("onbekende monitor-instelling: {other}"))?,
            ),
        };
        let mut me = Self {
            selector,
            monitors: Vec::new(),
        };
        me.refresh()?;
        Ok(me)
    }

    fn refresh(&mut self) -> Result<()> {
        self.monitors = Monitor::all().map_err(|e| anyhow!("monitors opvragen mislukt: {e}"))?;
        if self.monitors.is_empty() {
            return Err(anyhow!("geen monitors gevonden"));
        }
        Ok(())
    }

    /// Aantal monitors dat bij de huidige selectie hoort.
    pub fn describe(&self) -> String {
        let names: Vec<String> = self
            .selected()
            .iter()
            .map(|m| m.friendly_name().unwrap_or_else(|_| "?".into()))
            .collect();
        names.join(", ")
    }

    /// Of de huidige selectie precies één scherm bevat. De pipeline gebruikt
    /// dit om te bepalen of UIA veilig als tekstbron voor een frame kan dienen:
    /// UIA beschrijft alleen het voorgrondvenster, niet een tweede monitor.
    pub fn is_single_monitor(&self) -> bool {
        self.selected().len() == 1
    }

    fn selected(&self) -> Vec<&Monitor> {
        match self.selector {
            MonitorSelector::All => self.monitors.iter().collect(),
            MonitorSelector::Index(i) => self.monitors.iter().skip(i).take(1).collect(),
            MonitorSelector::Primary => {
                let primary = self
                    .monitors
                    .iter()
                    .find(|m| m.is_primary().unwrap_or(false));
                match primary {
                    Some(m) => vec![m],
                    None => self.monitors.iter().take(1).collect(),
                }
            }
        }
    }

    /// Eén screenshot per geselecteerde monitor.
    pub fn capture(&mut self) -> Result<Vec<Shot>> {
        match self.capture_once() {
            Ok(shots) if !shots.is_empty() => Ok(shots),
            // Monitor losgekoppeld of modus gewijzigd: cache verversen en nog
            // één poging, daarna geven we de fout door.
            _ => {
                self.refresh()?;
                let shots = self.capture_once()?;
                if shots.is_empty() {
                    Err(anyhow!("screenshot leverde geen frames op"))
                } else {
                    Ok(shots)
                }
            }
        }
    }

    fn capture_once(&self) -> Result<Vec<Shot>> {
        let mut out = Vec::new();
        for m in self.selected() {
            let name = m.friendly_name().unwrap_or_else(|_| "monitor".into());
            match m.capture_image() {
                Ok(image) => out.push(Shot {
                    image,
                    monitor: name,
                }),
                Err(e) => tracing::warn!(monitor = %name, error = %e, "screenshot mislukt"),
            }
        }
        Ok(out)
    }
}
