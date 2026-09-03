//! Automatisch opstarten bij het inloggen, via de `Run`-sleutel in het
//! per-gebruiker registerdeel (`HKCU`) — dezelfde plek die de meeste
//! achtergrond-apps gebruiken. Geen Taakplanner nodig en geen admin-rechten:
//! `HKEY_CURRENT_USER` is altijd schrijfbaar voor de ingelogde gebruiker.

use anyhow::Result;
use std::io;
use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
use winreg::RegKey;

const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Chronicle";

/// De opdrachtregel die bij het inloggen wordt uitgevoerd: dezelfde binary,
/// met het systemtray-icoon aan.
fn command_line() -> io::Result<String> {
    let exe = std::env::current_exe()?;
    Ok(format!("\"{}\" start --tray", exe.display()))
}

/// Staat er een `Chronicle`-waarde in de `Run`-sleutel? Elke leesfout (de
/// sleutel bestaat nog niet, geen waarde) betekent gewoon "nee".
pub fn is_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_SUBKEY)
        .and_then(|key| key.get_value::<String, _>(VALUE_NAME))
        .is_ok_and(|v| !v.trim().is_empty())
}

/// Schrijft de opstartopdracht weg. Maakt de `Run`-sleutel aan als die nog
/// niet bestaat (normaal is dat al zo — Windows gebruikt 'm zelf).
pub fn enable() -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_SUBKEY)?;
    key.set_value(VALUE_NAME, &command_line()?)?;
    Ok(())
}

/// Verwijdert de waarde weer. Bestond de sleutel of waarde al niet, dan is
/// het resultaat toch gewoon "uitgeschakeld" — geen fout.
pub fn disable() -> Result<()> {
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_SUBKEY, KEY_WRITE) {
        Ok(key) => match key.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
