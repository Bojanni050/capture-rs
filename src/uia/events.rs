//! Event handlers voor UI Automation structurele en property veranderingen.
//!
//! Deze module implementeert `IUIAutomationStructureChangedEventHandler` en
//! `IUIAutomationPropertyChangedEventHandler` via `windows::core::implement`.
//! Alleen lezen bij echte veranderingen — geen polling.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED, SAFEARRAY};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationPropertyChangedEventHandler, IUIAutomationPropertyChangedEventHandler_Impl,
    IUIAutomationStructureChangedEventHandler, IUIAutomationStructureChangedEventHandler_Impl,
    StructureChangeType, TreeScope_Subtree, UIA_ControlTypePropertyId, UIA_NamePropertyId,
    UIA_PROPERTY_ID,
};
use windows::core::{Ref, implement};

use super::reader::{UiaReader, WindowRead};

/// Berichten die van de COM callback naar de hoofdthread gestuurd worden.
#[derive(Debug, Clone)]
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

// ---------------------------------------------------------------------------
// COM handlers
// ---------------------------------------------------------------------------

#[implement(IUIAutomationStructureChangedEventHandler)]
struct StructureChangedHandler {
    sender: Sender<UiaEvent>,
    hwnd: isize,
}

impl IUIAutomationStructureChangedEventHandler_Impl for StructureChangedHandler_Impl {
    #[allow(non_snake_case)]
    fn HandleStructureChangedEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        changeType: StructureChangeType,
        _runtimeId: *const SAFEARRAY,
    ) -> windows::core::Result<()> {
        // `self` is de generated `_Impl` wrapper; toegang tot originele fields via `self`?
        // De `implement` macro dereft naar inner; we kunnen via `self` de velden bereiken.
        // Voor windows 0.62 is `self.sender` en `self.hwnd` direct beschikbaar via deref.
        let event = UiaEvent::StructureChanged {
            hwnd: self.hwnd,
            change_type: changeType,
        };
        // Channel vol/gesloten is geen COM-fout — gewoon Ok terug.
        let _ = self.sender.send(event);
        Ok(())
    }
}

#[implement(IUIAutomationPropertyChangedEventHandler)]
struct PropertyChangedHandler {
    sender: Sender<UiaEvent>,
    hwnd: isize,
}

impl IUIAutomationPropertyChangedEventHandler_Impl for PropertyChangedHandler_Impl {
    #[allow(non_snake_case)]
    fn HandlePropertyChangedEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        propertyId: UIA_PROPERTY_ID,
        _newValue: &VARIANT,
    ) -> windows::core::Result<()> {
        let event = UiaEvent::PropertyChanged {
            hwnd: self.hwnd,
            property_id: propertyId,
        };
        let _ = self.sender.send(event);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

pub struct UiaEventManager {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    event_sender: Sender<UiaEvent>,
    structure_handlers: Mutex<HashMap<isize, IUIAutomationStructureChangedEventHandler>>,
    property_handlers: Mutex<HashMap<isize, IUIAutomationPropertyChangedEventHandler>>,
}

impl UiaEventManager {
    /// Creëer manager met bestaande automation objecten (gedeeld met `UiaReader`).
    pub fn new(
        automation: IUIAutomation,
        cache: IUIAutomationCacheRequest,
        event_sender: Sender<UiaEvent>,
    ) -> Result<Self> {
        Ok(Self {
            automation,
            cache,
            event_sender,
            structure_handlers: Mutex::new(HashMap::new()),
            property_handlers: Mutex::new(HashMap::new()),
        })
    }

    /// Registreer handlers voor een venster.
    pub fn register_window_events(&self, hwnd: HWND) -> Result<()> {
        let hwnd_val = hwnd.0 as isize;
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        // StructureChanged
        let sc_handler: IUIAutomationStructureChangedEventHandler =
            StructureChangedHandler {
                sender: self.event_sender.clone(),
                hwnd: hwnd_val,
            }
            .into();
        unsafe {
            self.automation
                .AddStructureChangedEventHandler(&root, TreeScope_Subtree, &self.cache, &sc_handler)
                .context("AddStructureChangedEventHandler faalde")?;
        }
        self.structure_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hwnd_val, sc_handler);

        // PropertyChanged (Name + ControlType)
        let pc_handler: IUIAutomationPropertyChangedEventHandler = PropertyChangedHandler {
            sender: self.event_sender.clone(),
            hwnd: hwnd_val,
        }
        .into();
        let props = [UIA_NamePropertyId, UIA_ControlTypePropertyId];
        unsafe {
            self.automation
                .AddPropertyChangedEventHandlerNativeArray(
                    &root,
                    TreeScope_Subtree,
                    &self.cache,
                    &pc_handler,
                    &props,
                )
                .context("AddPropertyChangedEventHandler faalde")?;
        }
        self.property_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hwnd_val, pc_handler);

        Ok(())
    }

    pub fn unregister_window_events(&self, hwnd: HWND) -> Result<()> {
        let hwnd_val = hwnd.0 as isize;
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        if let Some(h) = self
            .structure_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hwnd_val)
        {
            unsafe {
                let _ = self
                    .automation
                    .RemoveStructureChangedEventHandler(&root, &h);
            }
        }
        if let Some(h) = self
            .property_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hwnd_val)
        {
            unsafe {
                let _ = self
                    .automation
                    .RemovePropertyChangedEventHandler(&root, &h);
            }
        }
        Ok(())
    }

    pub fn unregister_all(&self) -> Result<()> {
        let structure = std::mem::take(&mut *self
            .structure_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner()));
        for (hwnd_val, h) in structure {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            if let Ok(root) = unsafe { self.automation.ElementFromHandle(hwnd) } {
                unsafe {
                    let _ = self
                        .automation
                        .RemoveStructureChangedEventHandler(&root, &h);
                }
            }
        }
        let property = std::mem::take(&mut *self
            .property_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner()));
        for (hwnd_val, h) in property {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            if let Ok(root) = unsafe { self.automation.ElementFromHandle(hwnd) } {
                unsafe {
                    let _ = self
                        .automation
                        .RemovePropertyChangedEventHandler(&root, &h);
                }
            }
        }
        Ok(())
    }

    /// Lees na een event — gebruikt een verse reader (MTA-veilig).
    pub fn read_window_after_event(&self, hwnd: HWND) -> Result<WindowRead> {
        // Clone om te bewijzen dat COM objecten nog leven; echte lezing via nieuwe reader.
        let _ = self.automation.clone();
        let _ = self.cache.clone();
        let reader = UiaReader::new(1_500)?;
        reader.read_window(hwnd)
    }

    #[allow(dead_code)]
    pub fn send_event(&self, event: UiaEvent) -> Result<()> {
        self.event_sender
            .send(event)
            .map_err(|e| anyhow::anyhow!("Kon event niet verzenden: {e}"))
    }
}

impl Drop for UiaEventManager {
    fn drop(&mut self) {
        let _ = self.unregister_all();
    }
}

/// Dedicated MTA thread met message pump — nodig voor STA callbacks; voor MTA
/// is het een park-thread maar schaadt niet.
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
            loop {
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
                    };
                    let mut msg = MSG::default();
                    let ret = GetMessageW(&mut msg, None, 0, 0);
                    if ret.0 == -1 {
                        tracing::warn!("GetMessageW faalde op uia-events thread");
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    if ret.0 == 0 {
                        break;
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
        let e1 = UiaEvent::StructureChanged {
            hwnd: 123,
            change_type: StructureChangeType(0),
        };
        let e2 = UiaEvent::PropertyChanged {
            hwnd: 456,
            property_id: UIA_NamePropertyId,
        };
        assert!(matches!(e1, UiaEvent::StructureChanged { .. }));
        assert!(matches!(e2, UiaEvent::PropertyChanged { .. }));
    }

    #[test]
    fn test_event_sender_via_channel() {
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
