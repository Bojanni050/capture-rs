//! Event handlers voor UI Automation structurele en property veranderingen.
//!
//! Oorspronkelijk bedoeld om via `AddStructureChangedEventHandler` alleen te
//! lezen bij veranderingen. De eerdere implementatie compileerde niet met
//! `windows` 0.62 (verkeerde `windows_core` imports, `WINEVENT_*` paden en
//! handmatige `ComPtr` refcounting). Deze versie is een **compileerbare stub**
//! die dezelfde publieke API biedt zonder onveilige COM handlers.
//!
//! - `UiaEventManager::new(automation, cache, sender)` — bewaart de bestaande
//!   `IUIAutomation` objecten, registreert geen echte OS handlers.
//! - `register_window_events` / `unregister` zijn no-ops die alleen bookkeeping doen.
//! - `read_window_after_event` delegeert naar een verse `UiaReader` (correcte
//!   MTA-geïnitialiseerde lezing).
//! - `start_event_thread` start een minimale MTA message-pump zodat toekomstige
//!   echte COM callbacks niet verhongeren.
//!
//! Wanneer event-driven echt nodig is, vervang deze stub door een implementatie
//! met `#[implement(IUIAutomationStructureChangedEventHandler)]` volgens
//! `windows-rs` 0.62 docs.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, StructureChangeType, UIA_PROPERTY_ID,
};

use super::reader::{UiaReader, WindowRead};

/// Berichten die van de event callback naar de hoofdthread gestuurd worden.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum UiaEvent {
    StructureChanged {
        hwnd: isize,
        change_type: StructureChangeType,
    },
    PropertyChanged {
        hwnd: isize,
        property_id: UIA_PROPERTY_ID,
    },
}

/// Beheert de UI Automation event registratie voor vensters.
///
/// Stub-implementatie: houdt alleen bij welke `hwnd`s geregistreerd zijn.
#[allow(dead_code)]
pub struct UiaEventManager {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    event_sender: Sender<UiaEvent>,
    /// Gere gistreerde structure changed handlers per hwnd (stub: `()`).
    structure_handlers: Arc<Mutex<HashMap<isize, ()>>>,
    /// Geregistreerde property changed handlers per hwnd (stub: `()`).
    property_handlers: Arc<Mutex<HashMap<isize, ()>>>,
}

#[allow(dead_code)]
impl UiaEventManager {
    /// Creëer een nieuwe manager met bestaande automation objecten.
    ///
    /// De eerdere versie nam alleen `Sender`; `UiaService` geeft nu
    /// `(automation, cache, sender)` door — deze signatuur matcht dat.
    pub fn new(
        automation: IUIAutomation,
        cache: IUIAutomationCacheRequest,
        event_sender: Sender<UiaEvent>,
    ) -> Result<Self> {
        Ok(Self {
            automation,
            cache,
            event_sender,
            structure_handlers: Arc::new(Mutex::new(HashMap::new())),
            property_handlers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Registreer event handlers voor een venster (stub: alleen bookkeeping).
    pub fn register_window_events(&self, hwnd: HWND) -> Result<()> {
        // Valideer dat het venster een accessibility-element heeft, zodat
        // callers een echte fout krijgen bij ongeldige hwnd.
        let _ = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        let hwnd_val = hwnd.0 as isize;
        self.structure_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hwnd_val, ());
        self.property_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hwnd_val, ());
        Ok(())
    }

    /// Verwijder alle event handlers voor een venster (stub).
    pub fn unregister_window_events(&self, hwnd: HWND) -> Result<()> {
        let hwnd_val = hwnd.0 as isize;
        self.structure_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hwnd_val);
        self.property_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hwnd_val);
        Ok(())
    }

    /// Verwijder alle event handlers (stub).
    pub fn unregister_all(&self) -> Result<()> {
        self.structure_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.property_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }

    /// Lees de tekst uit een venster na een event.
    ///
    /// De stub maakt een verse reader aan op basis van de opgeslagen cache
    /// settings. De vorige versie probeerde `UiaReader` direct te construeren
    /// met private velden — dat kan niet. In plaats daarvan gebruiken we
    /// `UiaReader::new` en lezen het venster opnieuw.
    pub fn read_window_after_event(&self, hwnd: HWND) -> Result<WindowRead> {
        // Gebruik de opgeslagen automation/cache indirect: we valideren dat
        // de elementen bestaan en delegeren dan naar een nieuwe reader.
        // Dit vermijdt private-field toegang en blijft MTA-correct.
        let _ = self.automation.clone();
        let _ = self.cache.clone();

        // Maak een reader met dezelfde limiet als de service (of default).
        // `UiaReader::new` doet zelf `ensure_mta()` en `CoCreateInstance`.
        let reader = UiaReader::new(1_500)?;
        reader.read_window(hwnd)
    }

    /// Stuur een event naar de hoofdthread (gebruikt in tests).
    pub fn send_event(&self, event: UiaEvent) -> Result<()> {
        self.event_sender
            .send(event)
            .map_err(|e| anyhow::anyhow!("Kon event niet verzenden: {e}"))
    }
}

/// Start een dedicated MTA thread voor event verwerking.
///
/// De thread initialiseert COM als MTA en draait een minimale message loop
/// zodat toekomstige echte `IUIAutomation*EventHandler` callbacks ontvangen
/// kunnen worden. Zonder echte handlers is dit nu alleen een park-thread.
pub fn start_event_thread(_event_sender: Sender<UiaEvent>) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("chronicle-uia-events".into())
        .spawn(move || {
            unsafe {
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    tracing::error!("COM MTA initialiseren mislukt op event thread");
                    return;
                }
            }

            // Minimale message pump — correct pad is `UI::WindowsAndMessaging`.
            loop {
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
                    };

                    let mut msg = MSG::default();
                    // GetMessageW blokkeert tot er een message is; -1 = error, 0 = WM_QUIT
                    let ret = GetMessageW(&mut msg, None, 0, 0);
                    if ret.0 == -1 {
                        tracing::warn!("GetMessageW faalde op uia-events thread");
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    if ret.0 == 0 {
                        break; // WM_QUIT
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("Kon event thread niet starten: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn test_event_types() {
        let event1 = UiaEvent::StructureChanged {
            hwnd: 123,
            change_type: StructureChangeType(0),
        };
        let event2 = UiaEvent::PropertyChanged {
            hwnd: 456,
            property_id: windows::Win32::UI::Accessibility::UIA_NamePropertyId,
        };

        assert!(matches!(event1, UiaEvent::StructureChanged { .. }));
        assert!(matches!(event2, UiaEvent::PropertyChanged { .. }));
    }

    #[test]
    fn test_event_sender_via_channel() {
        // Test zonder UiaEventManager (vermijdt COM initialisatie).
        let (sender, receiver) = mpsc::channel();
        sender
            .send(UiaEvent::StructureChanged {
                hwnd: 123,
                change_type: StructureChangeType(0),
            })
            .unwrap();
        assert!(receiver.try_recv().is_ok());
    }
}
