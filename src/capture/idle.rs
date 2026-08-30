//! Hoe lang heb je geen toets aangeraakt en de muis niet bewogen?
//!
//! De eerste en goedkoopste ruisfilter: als je weg bent van je bureau heeft
//! het geen zin om elke vier seconden hetzelfde stilstaande scherm te OCR'en.

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// Seconden sinds de laatste toetsaanslag of muisbeweging.
///
/// `GetLastInputInfo` en `GetTickCount` gebruiken allebei een 32-bits
/// milliseconden-teller die na ~49 dagen uptime overloopt; `wrapping_sub`
/// geeft dan nog steeds het juiste verschil.
pub fn idle_seconds() -> u64 {
    unsafe {
        let mut lii = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !GetLastInputInfo(&mut lii).as_bool() {
            return 0;
        }
        let now = GetTickCount();
        (now.wrapping_sub(lii.dwTime) / 1000) as u64
    }
}
