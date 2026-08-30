//! STA-thread met een echt message-only venster: de betrouwbare manier om
//! UI Automation events te ontvangen.
//!
//! UIA-clients die events willen ontvangen horen — volgens Microsofts eigen
//! voorbeeldcode, en bevestigd door hoe screenpipe's eigen
//! `windows_uia.rs` het doet — in een Single-Threaded Apartment (STA) te
//! zitten, met een thread die Windows-berichten pompt. Zonder die pomp komen
//! de COM-callbacks van de provider (de doelapp) niet betrouwbaar aan: COM
//! marshalt STA-callbacks via de berichtenwachtrij van de apartment-thread.
//! Een eerdere versie initialiseerde deze thread als MTA en pompte een
//! wachtrij waar niemand ooit iets in postte — dat deed feitelijk niets.
//!
//! Deze versie maakt een verborgen `HWND_MESSAGE`-venster aan. Dat venster
//! geeft andere threads een adres om commando's naartoe te posten
//! (`PostMessageW`), en de message-loop die dat venster bedient is precies
//! wat COM nodig heeft om events af te leveren.
//!
//! Alle COM-objecten — de `IUIAutomation`-instantie, de cache, de
//! handler-registraties — leven uitsluitend op deze ene thread: STA-objecten
//! zijn apartment-gebonden en mogen niet cross-thread gedeeld worden zonder
//! marshaling. Andere threads raken ze daarom nooit rechtstreeks aan, alleen
//! via `PostMessageW` (commando's erin) en het `Sender<UiaEvent>`-kanaal
//! (events eruit).

use anyhow::{Context, Result, anyhow};
use std::sync::mpsc::Sender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest,
    IUIAutomationElement, IUIAutomationPropertyChangedEventHandler,
    IUIAutomationPropertyChangedEventHandler_Impl, IUIAutomationStructureChangedEventHandler,
    IUIAutomationStructureChangedEventHandler_Impl, StructureChangeType, TreeScope_Subtree,
    UIA_ControlTypePropertyId, UIA_IsOffscreenPropertyId, UIA_NamePropertyId, UIA_PROPERTY_ID,
    UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, TranslateMessage, WM_APP, WM_DESTROY,
    WM_NCCREATE, WNDCLASSEXW, WNDCLASS_STYLES, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, Ref, implement};

/// Berichten die van de COM callback naar `uia::UiaService` gestuurd worden.
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
// COM handlers — draaien op de STA-thread, sturen alleen platte data door.
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
// Toestand die uitsluitend op de STA-thread leeft.
// ---------------------------------------------------------------------------

/// Alles wat de window procedure nodig heeft, achter een `GWLP_USERDATA`-
/// pointer. Er is precies één eigenaar (de STA-thread zelf) en precies één
/// lezer (dezelfde thread, via de window procedure) — geen `Mutex` nodig,
/// Win32 garandeert al dat berichten voor dit venster sequentieel aankomen.
struct ThreadState {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    event_sender: Sender<UiaEvent>,
    structure_handlers: std::collections::HashMap<isize, IUIAutomationStructureChangedEventHandler>,
    property_handlers: std::collections::HashMap<isize, IUIAutomationPropertyChangedEventHandler>,
}

impl ThreadState {
    fn register(&mut self, hwnd: HWND) -> Result<()> {
        let hwnd_val = hwnd.0 as isize;
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        let sc_handler: IUIAutomationStructureChangedEventHandler = StructureChangedHandler {
            sender: self.event_sender.clone(),
            hwnd: hwnd_val,
        }
        .into();
        unsafe {
            self.automation
                .AddStructureChangedEventHandler(&root, TreeScope_Subtree, &self.cache, &sc_handler)
                .context("AddStructureChangedEventHandler faalde")?;
        }
        self.structure_handlers.insert(hwnd_val, sc_handler);

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
        self.property_handlers.insert(hwnd_val, pc_handler);

        Ok(())
    }

    fn unregister(&mut self, hwnd: HWND) {
        let hwnd_val = hwnd.0 as isize;
        let Ok(root) = (unsafe { self.automation.ElementFromHandle(hwnd) }) else {
            // Venster is al weg; de registratie is dan sowieso al waardeloos.
            self.structure_handlers.remove(&hwnd_val);
            self.property_handlers.remove(&hwnd_val);
            return;
        };
        if let Some(h) = self.structure_handlers.remove(&hwnd_val) {
            unsafe {
                let _ = self.automation.RemoveStructureChangedEventHandler(&root, &h);
            }
        }
        if let Some(h) = self.property_handlers.remove(&hwnd_val) {
            unsafe {
                let _ = self.automation.RemovePropertyChangedEventHandler(&root, &h);
            }
        }
    }

    fn unregister_all(&mut self) {
        let hwnds: Vec<isize> = self
            .structure_handlers
            .keys()
            .chain(self.property_handlers.keys())
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        for hwnd_val in hwnds {
            self.unregister(HWND(hwnd_val as *mut core::ffi::c_void));
        }
    }
}

// ---------------------------------------------------------------------------
// Message-only venster: het adres waar andere threads commando's naartoe
// posten, en tegelijk de message-loop die COM-events laat aankomen.
// ---------------------------------------------------------------------------

const WM_UIA_REGISTER: u32 = WM_APP + 1;
const WM_UIA_UNREGISTER: u32 = WM_APP + 2;
const WM_UIA_SHUTDOWN: u32 = WM_APP + 3;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Komt binnen vóórdat `CreateWindowExW` teruggeeft; dit is de
        // standaard Win32-manier om toestand aan een net aangemaakt venster
        // te koppelen (de state-pointer reist mee als `lpCreateParams`).
        WM_NCCREATE => {
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_UIA_REGISTER => {
            if let Some(state) = state_from(hwnd) {
                let target = HWND(lparam.0 as *mut core::ffi::c_void);
                if let Err(e) = state.register(target) {
                    tracing::debug!(error = %e, "uia event registratie mislukt");
                }
            }
            LRESULT(0)
        }
        WM_UIA_UNREGISTER => {
            if let Some(state) = state_from(hwnd) {
                let target = HWND(lparam.0 as *mut core::ffi::c_void);
                state.unregister(target);
            }
            LRESULT(0)
        }
        WM_UIA_SHUTDOWN => {
            if let Some(state) = state_from(hwnd) {
                state.unregister_all();
            }
            // `DestroyWindow` levert WM_DESTROY synchroon af op deze zelfde
            // thread, dus de opruiming hieronder volgt meteen hierna.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            if ptr != 0 {
                // Neemt de Box terug in eigendom en laat hem meteen vallen —
                // zonder dit lekt de toestand (en de COM-objecten erin) bij
                // elke herstart.
                drop(unsafe { Box::from_raw(ptr as *mut ThreadState) });
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn state_from<'a>(hwnd: HWND) -> Option<&'a mut ThreadState> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr == 0 {
        None
    } else {
        Some(unsafe { &mut *(ptr as *mut ThreadState) })
    }
}

/// Draait op de toegewijde thread: COM als STA initialiseren, het venster
/// aanmaken, en dan de message-loop pompen tot `WM_UIA_SHUTDOWN`.
fn run(event_sender: Sender<UiaEvent>, ready: std::sync::mpsc::Sender<Result<isize>>) {
    let outcome = (|| -> Result<HWND> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .context("COM STA initialiseren mislukt op uia-events thread")?;
        }

        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .context("UI Automation niet beschikbaar op uia-events thread")?;
        let cache = unsafe { automation.CreateCacheRequest() }
            .context("cacheverzoek aanmaken mislukt op uia-events thread")?;
        unsafe {
            cache.AddProperty(UIA_NamePropertyId)?;
            cache.AddProperty(UIA_ControlTypePropertyId)?;
            cache.AddProperty(UIA_IsOffscreenPropertyId)?;
            cache.AddPattern(UIA_ValuePatternId)?;
            cache.SetTreeScope(TreeScope_Subtree)?;
            cache.SetAutomationElementMode(AutomationElementMode_None)?;
        }

        let state = Box::new(ThreadState {
            automation,
            cache,
            event_sender,
            structure_handlers: std::collections::HashMap::new(),
            property_handlers: std::collections::HashMap::new(),
        });
        let state_ptr = Box::into_raw(state);

        let hinstance: windows::Win32::Foundation::HINSTANCE =
            unsafe { GetModuleHandleW(None) }
                .context("modulehandvat opvragen mislukt")?
                .into();

        let class_name: Vec<u16> = "ChronicleUiaEvents\0".encode_utf16().collect();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            // De boxed state zou anders nooit meer vrijkomen.
            drop(unsafe { Box::from_raw(state_ptr) });
            return Err(anyhow!("uia-events vensterklasse registreren mislukt"));
        }

        let window_name: Vec<u16> = "ChronicleUiaEventsWindow\0".encode_utf16().collect();
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(window_name.as_ptr()),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance),
                Some(state_ptr as *const core::ffi::c_void),
            )
        };
        match hwnd {
            Ok(hwnd) => Ok(hwnd),
            Err(e) => {
                drop(unsafe { Box::from_raw(state_ptr) });
                Err(e).context("uia-events venster aanmaken mislukt")
            }
        }
    })();

    let hwnd = match outcome {
        Ok(hwnd) => {
            let _ = ready.send(Ok(hwnd.0 as isize));
            hwnd
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = hwnd;

    loop {
        let mut msg = MSG::default();
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Handvat voor de STA-eventthread. `register_window_events` en
/// `unregister_window_events` posten alleen een bericht — de daadwerkelijke
/// COM-aanroepen gebeuren op de eventthread zelf, nooit hier.
pub struct UiaEventThread {
    hwnd: isize,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl UiaEventThread {
    pub fn start(event_sender: Sender<UiaEvent>) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize>>();

        let join_handle = std::thread::Builder::new()
            .name("chronicle-uia-events".into())
            .spawn(move || run(event_sender, ready_tx))
            .context("uia-events thread starten mislukt")?;

        let hwnd = ready_rx
            .recv()
            .map_err(|_| anyhow!("uia-events thread startte niet"))??;

        Ok(Self {
            hwnd,
            join_handle: Some(join_handle),
        })
    }

    pub fn register_window_events(&self, hwnd: HWND) -> Result<()> {
        self.post(WM_UIA_REGISTER, hwnd)
    }

    pub fn unregister_window_events(&self, hwnd: HWND) -> Result<()> {
        self.post(WM_UIA_UNREGISTER, hwnd)
    }

    fn post(&self, msg: u32, hwnd: HWND) -> Result<()> {
        let target = HWND(self.hwnd as *mut core::ffi::c_void);
        unsafe { PostMessageW(Some(target), msg, WPARAM(0), LPARAM(hwnd.0 as isize)) }
            .context("bericht naar uia-events thread posten mislukt")
    }
}

impl Drop for UiaEventThread {
    fn drop(&mut self) {
        let target = HWND(self.hwnd as *mut core::ffi::c_void);
        unsafe {
            let _ = PostMessageW(Some(target), WM_UIA_SHUTDOWN, WPARAM(0), LPARAM(0));
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
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
