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
pub fn ensure_mta() -> Result<()> {
    static DONE: OnceLock<bool> = OnceLock::new();

    let ok = *DONE.get_or_init(|| unsafe {
        match CoIncrementMTAUsage() {
            Ok(_cookie) => {
                // De cookie is een handvat om de MTA later af te bouwen; wij
                // willen hem juist houden zolang het proces leeft, dus we laten
                // hem vallen zonder er iets mee te doen.
                true
            }
            Err(e) => {
                tracing::error!(error = %e, "COM/MTA initialiseren mislukt");
                false
            }
        }
    });

    if ok {
        Ok(())
    } else {
        Err(anyhow!("COM/MTA kon niet worden geïnitialiseerd"))
    }
}
