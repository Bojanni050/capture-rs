//! Event handlers voor UI Automation structurele en property veranderingen.
//!
//! Deze module implementeert AddStructureChangedEventHandler en AddPropertyChangedEventHandler
//! om de accessibility-boom alleen uit te lezen wanneer de app zelf meldt dat er iets veranderd is.
//!
//! Dit is architectonisch beter dan periodieke boomwandeling:
//! - Geen polling meer nodig
//! - Alleen lezen wanneer er daadwerkelijk veranderingen zijn
//! - Minder CPU en COM overhead
//!
//! Let op: niet alle apps sturen betrouwbare events. Voor die apps valt het
//! systeem terug op de traditionele polling methode.

use anyhow::{anyhow, Context, Result};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest,
    IUIAutomationElement, IUIAutomationPropertyChangedEventHandler,
    IUIAutomationPropertyChangedEventHandler_Vtbl, IUIAutomationStructureChangedEventHandler,
    IUIAutomationStructureChangedEventHandler_Vtbl, StructureChangeType, TreeScope_Subtree,
    UIA_ControlTypePropertyId, UIA_NamePropertyId, UIA_PROPERTY_ID,
};
use windows_core::{implement_com_interface, ComPtr, HRESULT, Error as WinError, Interface};

use super::reader::WindowRead;

/// Berichten die van de event callback naar de hoofdthread gestuurd worden.
#[derive(Debug, Clone)]
pub enum UiaEvent {
    /// De structuur van een element is veranderd (kinderen toegevoegd/verwijderd).
    StructureChanged {
        hwnd: isize,
        change_type: StructureChangeType,
    },
    /// Een property van een element is veranderd.
    PropertyChanged {
        hwnd: isize,
        property_id: UIA_PROPERTY_ID,
    },
}

/// Handler voor StructureChanged events.
/// 
/// Deze struct implementeert de COM interface IUIAutomationStructureChangedEventHandler.
/// Wanneer UIA een structurele verandering detecteert, roept het deze handler aan.
pub struct StructureChangedHandler {
    sender: Sender<UiaEvent>,
    hwnd: isize,
}

impl StructureChangedHandler {
    pub fn new(sender: Sender<UiaEvent>, hwnd: isize) -> ComPtr<Self> {
        let handler = Self { sender, hwnd };
        unsafe {
            let ptr = Box::into_raw(Box::new(handler));
            ComPtr::from_raw(ptr as *mut _)
        }
    }
}

// Implementeer de COM interface trait voor StructureChangedHandler
impl IUIAutomationStructureChangedEventHandler_Impl for StructureChangedHandler {
    fn HandleStructureChangedEvent(
        &self,
        _sender: windows_core::Ref<IUIAutomationElement>,
        change_type: StructureChangeType,
        _runtime_id: *const windows::Win32::System::Com::SAFEARRAY,
    ) -> windows_core::Result<()> {
        // Stuur het event naar de hoofdthread
        let event = UiaEvent::StructureChanged {
            hwnd: self.hwnd,
            change_type,
        };
        
        if self.sender.send(event).is_err() {
            // Channel is gesloten, geef een COM fout terug
            return Err(WinError::from_win32(windows::Win32::Foundation::E_FAIL));
        }
        
        Ok(())
    }
}

// Implementeer de COM interface voor StructureChangedHandler
implement_com_interface!(
    StructureChangedHandler,
    IUIAutomationStructureChangedEventHandler,
    StructureChangedHandler_Vtbl
);

/// Handler voor PropertyChanged events.
/// 
/// Deze struct implementeert de COM interface IUIAutomationPropertyChangedEventHandler.
pub struct PropertyChangedHandler {
    sender: Sender<UiaEvent>,
    hwnd: isize,
}

impl PropertyChangedHandler {
    pub fn new(sender: Sender<UiaEvent>, hwnd: isize) -> ComPtr<Self> {
        let handler = Self { sender, hwnd };
        unsafe {
            let ptr = Box::into_raw(Box::new(handler));
            ComPtr::from_raw(ptr as *mut _)
        }
    }
}

// Implementeer de COM interface trait voor PropertyChangedHandler
impl IUIAutomationPropertyChangedEventHandler_Impl for PropertyChangedHandler {
    fn HandlePropertyChangedEvent(
        &self,
        _sender: windows_core::Ref<IUIAutomationElement>,
        property_id: UIA_PROPERTY_ID,
        _new_value: &windows::Win32::System::Variant::VARIANT,
    ) -> windows_core::Result<()> {
        // We zijn alleen geinteresseerd in Name en ControlType veranderingen
        // voor onze use case
        let event = UiaEvent::PropertyChanged {
            hwnd: self.hwnd,
            property_id,
        };
        
        if self.sender.send(event).is_err() {
            return Err(WinError::from_win32(windows::Win32::Foundation::E_FAIL));
        }
        
        Ok(())
    }
}

// Implementeer de COM interface voor PropertyChangedHandler
implement_com_interface!(
    PropertyChangedHandler,
    IUIAutomationPropertyChangedEventHandler,
    PropertyChangedHandler_Vtbl
);

/// Beheert de UI Automation event registratie voor vensters.
pub struct UiaEventManager {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    event_sender: Sender<UiaEvent>,
    /// Geregistreerde structure changed handlers per hwnd.
    structure_handlers: Arc<Mutex<std::collections::HashMap<isize, ComPtr<IUIAutomationStructureChangedEventHandler>>>>,
    /// Geregistreerde property changed handlers per hwnd.
    property_handlers: Arc<Mutex<std::collections::HashMap<isize, ComPtr<IUIAutomationPropertyChangedEventHandler>>>>,
}

impl UiaEventManager {
    /// Creëer een nieuwe UiaEventManager.
    pub fn new(event_sender: Sender<UiaEvent>) -> Result<Self> {
        // Initialiseer COM MTA voor deze thread
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .context("COM MTA initialiseren mislukt")?;
        }

        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .context("UI Automation is niet beschikbaar")?;

        let cache = unsafe { automation.CreateCacheRequest() }
            .context("UIA-cacheverzoek aanmaken mislukt")?;

        unsafe {
            // Precies de properties die we straks lezen.
            cache.AddProperty(UIA_NamePropertyId)?;
            cache.AddProperty(UIA_ControlTypePropertyId)?;
            cache.SetTreeScope(TreeScope_Subtree)?;
            cache.SetAutomationElementMode(AutomationElementMode_None)?;
        }

        Ok(Self {
            automation,
            cache,
            event_sender,
            structure_handlers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            property_handlers: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    /// Registreer event handlers voor een venster.
    /// 
    /// Registreert handlers voor:
    /// - StructureChanged events (kinderen toegevoegd/verwijderd)
    /// - PropertyChanged events voor Name en ControlType
    pub fn register_window_events(&self, hwnd: HWND) -> Result<()> {
        let hwnd_val = hwnd.0 as isize;
        
        // Haal het root element van het venster
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        // Maak en registreer structure changed handler
        let structure_handler = StructureChangedHandler::new(self.event_sender.clone(), hwnd_val);
        
        unsafe {
            self.automation.AddStructureChangedEventHandler(
                &root,
                TreeScope_Subtree,
                &self.cache,
                &structure_handler,
            )?;
        }

        // Sla de handler op
        {
            let mut handlers = self.structure_handlers.lock().unwrap();
            handlers.insert(hwnd_val, structure_handler);
        }

        // Maak en registreer property changed handler voor Name en ControlType
        let property_handler = PropertyChangedHandler::new(self.event_sender.clone(), hwnd_val);
        
        let properties = [UIA_NamePropertyId, UIA_ControlTypePropertyId];
        
        unsafe {
            self.automation.AddPropertyChangedEventHandlerNativeArray(
                &root,
                TreeScope_Subtree,
                &self.cache,
                &property_handler,
                &properties,
            )?;
        }

        // Sla de handler op
        {
            let mut handlers = self.property_handlers.lock().unwrap();
            handlers.insert(hwnd_val, property_handler);
        }

        Ok(())
    }

    /// Verwijder alle event handlers voor een venster.
    pub fn unregister_window_events(&self, hwnd: HWND) -> Result<()> {
        let hwnd_val = hwnd.0 as isize;
        
        // Haal het root element van het venster
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        // Verwijder structure changed handler
        {
            let mut handlers = self.structure_handlers.lock().unwrap();
            if let Some(handler) = handlers.remove(&hwnd_val) {
                unsafe {
                    self.automation.RemoveStructureChangedEventHandler(&root, &handler)?;
                }
            }
        }

        // Verwijder property changed handler
        {
            let mut handlers = self.property_handlers.lock().unwrap();
            if let Some(handler) = handlers.remove(&hwnd_val) {
                unsafe {
                    self.automation.RemovePropertyChangedEventHandler(&root, &handler)?;
                }
            }
        }

        Ok(())
    }

    /// Verwijder alle event handlers.
    pub fn unregister_all(&self) -> Result<()> {
        // Verwijder alle structure handlers
        {
            let mut handlers = self.structure_handlers.lock().unwrap();
            for (hwnd_val, handler) in handlers.drain() {
                let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
                if let Ok(root) = unsafe { self.automation.ElementFromHandle(hwnd) } {
                    unsafe {
                        let _ = self.automation.RemoveStructureChangedEventHandler(&root, &handler);
                    }
                }
            }
        }

        // Verwijder alle property handlers
        {
            let mut handlers = self.property_handlers.lock().unwrap();
            for (hwnd_val, handler) in handlers.drain() {
                let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
                if let Ok(root) = unsafe { self.automation.ElementFromHandle(hwnd) } {
                    unsafe {
                        let _ = self.automation.RemovePropertyChangedEventHandler(&root, &handler);
                    }
                }
            }
        }

        Ok(())
    }

    /// Lees de tekst uit een venster na een event.
    pub fn read_window_after_event(&self, hwnd: HWND) -> Result<WindowRead> {
        use super::reader::UiaReader;
        
        let reader = UiaReader {
            automation: self.automation.clone(),
            cache: self.cache.clone(),
            content_view: unsafe { self.automation.ContentViewCondition() }?,
            max_elements: 1500,
        };
        
        reader.read_window(hwnd)
    }
}

/// Start een dedicated MTA thread voor event verwerking.
/// 
/// Deze thread:
/// 1. Initialiseert COM als MTA (Multi-Threaded Apartment)
/// 2. Start een message loop om COM callbacks te ontvangen
/// 3. Blijft leven zolang de event_sender actief is
/// 
/// COM callbacks voor UI Automation worden alleen ontvangen op threads
/// die MTA geinitialiseerd zijn en een message loop hebben.
pub fn start_event_thread(
    _event_sender: Sender<UiaEvent>,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("chronicle-uia-events".into())
        .spawn(move || {
            // 1. Initialiseer COM als MTA
            unsafe {
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    tracing::error!("COM MTA initialiseren mislukt op event thread");
                    return;
                }
            }

            // 2. Start de COM message loop
            // Deze loop zorgt ervoor dat COM callbacks kunnen worden ontvangen
            loop {
                unsafe {
                    use windows::Win32::WindowsAndMessaging::{
                        MsgWaitForMultipleObjects, PeekMessageA, TranslateMessage, 
                        DispatchMessageA, QS_ALLINPUT, PM_REMOVE
                    };

                    // Wacht op COM messages of andere events
                    let result = MsgWaitForMultipleObjects(
                        0,                              // Geen handles om op te wachten
                        std::ptr::null(),              // Geen handles array
                        false,                         // Wacht niet op alle handles
                        u32::MAX,                     // Infinite timeout
                        QS_ALLINPUT,                  // Alle input messages
                    );

                    // WAIT_OBJECT_0 (0) betekent dat er een message beschikbaar is
                    if result == 0 {
                        let mut msg = std::mem::zeroed();
                        // Verwerk alle beschikbare messages
                        while PeekMessageA(
                            &mut msg,
                            None,
                            0,
                            0,
                            PM_REMOVE,
                        ).into() {
                            TranslateMessage(&msg);
                            DispatchMessageA(&msg);
                        }
                    } else {
                        // Timeout of error - check of we moeten stoppen
                        // (In de toekomst kunnen we hier een shutdown flag checken)
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("Kon event thread niet starten: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_types() {
        let event1 = UiaEvent::StructureChanged {
            hwnd: 123,
            change_type: StructureChangeType(0),
        };
        let event2 = UiaEvent::PropertyChanged {
            hwnd: 456,
            property_id: UIA_NamePropertyId,
        };
        
        // Test dat events gemaakt kunnen worden
        assert!(matches!(event1, UiaEvent::StructureChanged { .. }));
        assert!(matches!(event2, UiaEvent::PropertyChanged { .. }));
    }

    #[test]
    fn test_event_sender() {
        let (sender, receiver) = mpsc::channel();
        let manager = UiaEventManager {
            automation: unsafe { std::mem::zeroed() },
            cache: unsafe { std::mem::zeroed() },
            event_sender: sender,
            structure_handlers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            property_handlers: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };
        
        // Test dat we een event kunnen verzenden
        assert!(manager.send_event(UiaEvent::StructureChanged {
            hwnd: 123,
            change_type: StructureChangeType(0)
        }).is_ok());
        
        // Controleer dat het event ontvangen is
        assert!(receiver.try_recv().is_ok());
    }
}

impl UiaEventManager {
    /// Stuur een event naar de hoofdthread.
    pub fn send_event(&self, event: UiaEvent) -> Result<()> {
        self.event_sender.send(event)
            .map_err(|e| anyhow::anyhow!("Kon event niet verzenden: {}", e))
    }
}
