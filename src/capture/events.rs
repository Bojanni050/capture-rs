//! Signalen van Windows die aangeven dat de actieve context veranderde.
//!
//! Dit is bewust slechts een *wekker* voor de pipeline: een foreground-event
//! zegt niet dat de inhoud al stabiel is of dat pixels zijn gewijzigd. De
//! pipeline past daarom daarna nog steeds alle bestaande filters toe.

use anyhow::{anyhow, Result};
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use windows::Win32::UI::Accessibility::{
    SetWinEventHook, UnhookWinEvent, EVENT_SYSTEM_FOREGROUND, HWINEVENTHOOK,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// Er kan maar één hook per proces actief zijn. Een globale sender is nodig
/// omdat de Win32-callback geen eigen contextpointer accepteert.
static FOREGROUND_TX: OnceLock<Mutex<Option<UnboundedSender<()>>>> = OnceLock::new();

fn sender_slot() -> &'static Mutex<Option<UnboundedSender<()>>> {
    FOREGROUND_TX.get_or_init(|| Mutex::new(None))
}

/// Houdt de Win32-hook in leven. Bij `Drop` wordt de hook weer afgemeld.
pub struct ForegroundEvents {
    hook: HWINEVENTHOOK,
}

impl ForegroundEvents {
    /// Abonneert op wijzigingen van het voorgrondvenster.
    ///
    /// Het kanaal heeft geen payload: de pipeline leest zelf de actuele
    /// vensterinformatie, zodat een oude eventmelding nooit wordt verwerkt.
    pub fn start() -> Result<(Self, UnboundedReceiver<()>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        *sender_slot()
            .lock()
            .map_err(|_| anyhow!("foreground-eventslot is vergrendeld"))? = Some(tx);

        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };

        if hook.is_invalid() {
            *sender_slot().lock().expect("foreground-eventslot poisoned") = None;
            return Err(windows::core::Error::from_win32().into());
        }

        Ok((Self { hook }, rx))
    }
}

impl Drop for ForegroundEvents {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.hook);
        }
        if let Ok(mut sender) = sender_slot().lock() {
            *sender = None;
        }
    }
}

unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: windows::Win32::Foundation::HWND,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    // `send` is niet-blokkerend. De pipeline voegt vervolgens meerdere snelle
    // meldingen samen met een debounce voordat er een capture wordt gemaakt.
    if let Ok(sender) = sender_slot().lock() {
        if let Some(tx) = sender.as_ref() {
            let _ = tx.send(());
        }
    }
}
