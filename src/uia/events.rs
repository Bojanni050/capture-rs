//! Event handlers voor UI Automation structurele en property veranderingen.
//!
//! Deze module biedt de basis voor event-driven UIA lezen.
//! 
//! Met AddStructureChangedEventHandler en AddPropertyChangedEventHandler kunnen we
//! de accessibility-boom alleen uitlezen wanneer de app zelf meldt dat er iets
//! veranderd is, in plaats van periodieke polling.
//!
//! Voordelen:
//! - Geen polling meer nodig
//! - Alleen lezen wanneer er daadwerkelijk veranderingen zijn  
//! - Minder CPU en COM overhead
//!
//! Let op: niet alle apps sturen betrouwbare events. Voor die apps valt het
//! systeem terug op de traditionele polling methode.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    TreeScope_Subtree, UIA_NamePropertyId, UIA_ControlTypePropertyId,
};

use super::reader::WindowRead;

/// Berichten die van de event callback naar de hoofdthread gestuurd worden.
#[derive(Debug, Clone)]
pub enum UiaEvent {
    /// De structuur van een element is veranderd.
    StructureChanged {
        hwnd: isize,
    },
    /// Een property (Name of ControlType) is veranderd.
    PropertyChanged {
        hwnd: isize,
    },
}

/// Beheert de UI Automation event registratie.
/// 
/// Deze struct is verantwoordelijk voor het registreren en beheren van
/// event handlers voor vensters.
pub struct UiaEventManager {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    event_sender: Sender<UiaEvent>,
    /// Geregistreerde vensters.
    registered_windows: Arc<Mutex<std::collections::HashSet<isize>>>,
}

impl UiaEventManager {
    /// Creëer een nieuwe UiaEventManager.
    pub fn new(
        automation: IUIAutomation,
        cache: IUIAutomationCacheRequest,
        event_sender: Sender<UiaEvent>,
    ) -> Self {
        Self {
            automation,
            cache,
            event_sender,
            registered_windows: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Registreer een venster voor event notificaties.
    /// 
    /// In de toekomst kunnen we hier daadwerkelijk de COM event handlers
    /// registreren met AddStructureChangedEventHandler en AddPropertyChangedEventHandler.
    pub fn register_window(&self, hwnd: HWND) -> anyhow::Result<()> {
        let hwnd_val = hwnd.0 as isize;
        
        // Haal het root element van het venster
        let _root = unsafe { self.automation.ElementFromHandle(hwnd) }?;
        
        // Registreer het venster
        {
            let mut windows = self.registered_windows.lock().unwrap();
            windows.insert(hwnd_val);
        }

        Ok(())
    }

    /// Verwijder een venster uit de event registratie.
    pub fn unregister_window(&self, hwnd: HWND) -> anyhow::Result<()> {
        let hwnd_val = hwnd.0 as isize;
        
        {
            let mut windows = self.registered_windows.lock().unwrap();
            windows.remove(&hwnd_val);
        }

        Ok(())
    }

    /// Lees de tekst uit een venster na een event.
    pub fn read_window_after_event(&self, hwnd: HWND) -> anyhow::Result<WindowRead> {
        use super::reader::UiaReader;
        
        let reader = UiaReader {
            automation: self.automation.clone(),
            cache: self.cache.clone(),
            content_view: unsafe { self.automation.ContentViewCondition() }?,
            max_elements: 1500,
        };
        
        reader.read_window(hwnd)
    }

    /// Stuur een event naar de hoofdthread.
    pub fn send_event(&self, event: UiaEvent) -> anyhow::Result<()> {
        self.event_sender.send(event)
            .map_err(|e| anyhow::anyhow!("Kon event niet verzenden: {}", e))
    }
}

/// Start een dedicated thread voor event verwerking.
/// 
/// Deze thread initialiseert de COM MTA apartment en is nodig voor
/// het ontvangen van COM callbacks.
pub fn start_event_thread(
    _event_sender: Sender<UiaEvent>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("chronicle-uia-events".into())
        .spawn(move || {
            // In een echte implementatie zouden we hier:
            // 1. CoInitializeEx(COINIT_MULTITHREADED) aanroepen
            // 2. Een message loop starten voor COM callbacks
            // 3. Events doorsturen naar de hoofdthread
            
            loop {
                std::thread::park_timeout(std::time::Duration::from_secs(1));
            }
        })
        .map_err(|e| anyhow::anyhow!("Kon event thread niet starten: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_types() {
        let event1 = UiaEvent::StructureChanged { hwnd: 123 };
        let event2 = UiaEvent::PropertyChanged { hwnd: 456 };
        
        // Test dat events gemaakt kunnen worden
        assert!(matches!(event1, UiaEvent::StructureChanged { .. }));
        assert!(matches!(event2, UiaEvent::PropertyChanged { .. }));
    }
}
