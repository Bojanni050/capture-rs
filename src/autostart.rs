//! Automatisch opstarten bij het inloggen, via de `Run`-sleutel in het
//! per-gebruiker registerdeel (`HKCU`) — dezelfde plek die de meeste
//! achtergrond-apps gebruiken. Geen Taakplanner nodig en geen admin-rechten:
//! `HKEY_CURRENT_USER` is altijd schrijfbaar voor de ingelogde gebruiker.

use anyhow::Result;
use std::io;
use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
use winreg::RegKey;

const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Capture";
/// Waarde van vóór de rebrand; wordt weggehaald zodat er niet twee entries
/// proberen te starten (de oude verwijst naar een exe die niet meer bestaat).
const LEGACY_VALUE_NAME: &str = "Chronicle";

/// De opdrachtregel die bij het inloggen wordt uitgevoerd: dezelfde binary,
/// met het systemtray-icoon aan.
fn command_line() -> io::Result<String> {
    let exe = std::env::current_exe()?;
    Ok(format!("\"{}\" start --tray", exe.display()))
}

/// Staat er een `Capture`-waarde in de `Run`-sleutel? Elke leesfout (de
/// sleutel bestaat nog niet, geen waarde) betekent gewoon "nee". De legacy
/// `Chronicle`-waarde telt niet mee: die verwijst naar de oude binary.
pub fn is_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_SUBKEY)
        .and_then(|key| key.get_value::<String, _>(VALUE_NAME))
        .is_ok_and(|v| !v.trim().is_empty())
}

/// Schrijft de opstartopdracht weg. Maakt de `Run`-sleutel aan als die nog
/// niet bestaat (normaal is dat al zo — Windows gebruikt 'm zelf). Ruimt
/// meteen de oude `Chronicle`-waarde op.
pub fn enable() -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_SUBKEY)?;
    key.set_value(VALUE_NAME, &command_line()?)?;
    let _ = delete_if_present(&key, LEGACY_VALUE_NAME);
    Ok(())
}

/// Verwijdert de waarde weer. Bestond de sleutel of waarde al niet, dan is
/// het resultaat toch gewoon "uitgeschakeld" — geen fout. De legacy
/// `Chronicle`-waarde gaat er ook uit, voor het geval die er nog staat.
pub fn disable() -> Result<()> {
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_SUBKEY, KEY_WRITE) {
        Ok(key) => {
            delete_if_present(&key, VALUE_NAME)?;
            delete_if_present(&key, LEGACY_VALUE_NAME)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Verwijdert een waarde; bestond die al niet, dan is dat geen fout.
fn delete_if_present(key: &RegKey, name: &str) -> Result<()> {
    match key.delete_value(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Zet de opstartopdracht van vóór de rebrand om naar de huidige binary.
/// Stond `Chronicle` aan, dan gaat die over naar `Capture` — anders zou
/// automatisch opstarten na de upgrade stilzwijgend stoppen, want de oude
/// exe bestaat niet meer. Idempotent en nooit fataal: lukt het niet, dan
/// laat de gebruiker het gewoon opnieuw aanzetten.
pub fn migrate_legacy() {
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_SUBKEY, KEY_WRITE)
    else {
        return;
    };
    if key
        .get_value::<String, _>(VALUE_NAME)
        .is_ok_and(|v| !v.trim().is_empty())
    {
        // De nieuwe waarde is leidend; de oude mag weg.
        let _ = key.delete_value(LEGACY_VALUE_NAME);
        return;
    }
    match key.get_value::<String, _>(LEGACY_VALUE_NAME) {
        Ok(old) if !old.trim().is_empty() => {}
        // Niets te migreren (sleutel of waarde bestaat niet).
        _ => return,
    }
    match command_line() {
        Ok(cmd) => match key.set_value(VALUE_NAME, &cmd) {
            // Pas de oude waarde opruimen als de nieuwe staat.
            Ok(()) => {
                let _ = key.delete_value(LEGACY_VALUE_NAME);
                tracing::info!("autostart verhuisd van Chronicle naar Capture");
            }
            Err(e) => tracing::warn!(error = %e, "autostart naar de nieuwe naam verhuizen mislukt"),
        },
        Err(e) => {
            tracing::warn!(error = %e, "eigen exe-locatie bepalen mislukt; autostart niet verhuisd")
        }
    }
}
