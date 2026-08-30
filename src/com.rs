//! Gedeelde COM-initialisatie.
//!
//! Zowel de OCR-engine (WinRT) als UI Automation (klassiek COM) hebben een
//! geïnitialiseerd apartment nodig. `CoIncrementMTAUsage` zorgt dat het proces
//! een MTA heeft; elke thread mag daarna COM gebruiken zonder zelf
//! `CoInitializeEx` aan te roepen.

use anyhow::{anyhow, Result};
use std::sync::OnceLock;
use windows::Win32::System::Com::CoIncrementMTAUsage;

/// Idempotent: de eerste aanroep doet het werk, de rest leest het resultaat.
///
/// Bij failure wordt niet gecached — een volgende call kan opnieuw proberen
/// (bv. COM was tijdelijk niet beschikbaar bij vroege startup).
pub fn ensure_mta() -> Result<()> {
    static DONE: OnceLock<Result<(), String>> = OnceLock::new();

    // Fast path: al succesvol geïnitialiseerd.
    if let Some(res) = DONE.get() {
        return res.clone().map_err(|e| anyhow!(e.clone()));
    }

    let res = unsafe {
        match CoIncrementMTAUsage() {
            Ok(_cookie) => Ok(()),
            Err(e) => {
                tracing::error!(error = %e, "COM/MTA initialiseren mislukt");
                Err(format!("COM/MTA initialiseren mislukt: {e}"))
            }
        }
    };

    // Alleen succes cachen; bij fout mag een volgende call opnieuw proberen.
    if res.is_ok() {
        let _ = DONE.set(res.clone().map_err(|e| e.clone()));
        // Clone voor return is goedkoop (Ok).
    }
    res.map_err(|e| anyhow!(e))
}
