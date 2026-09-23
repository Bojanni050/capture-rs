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
//! via `PostMessageW` (commando's erin) en een coalescende `watch`-zender
//! (events eruit).
//!
//! **Eén thread per geregistreerd venster, niet één gedeelde thread.**
//! `AddStructureChangedEventHandler`/`AddPropertyChangedEventHandlerNativeArray`
//! zijn synchrone COM-aanroepen zonder deadline. Bij sommige providers (met
//! name Electron/Chromium-vensters, waar het abonneren op structuurwijziging
//! de renderer dwingt tot volledige accessibility-mode) kunnen ze minutenlang
//! of voorgoed blijven hangen. Met één gedeelde thread voor alle vensters zou
//! zo'n hang de message-pump — en daarmee elk event, voor elk venster —
//! stilleggen voor de rest van de procesduur. `UiaEventThread::switch_window`
//! start daarom bij elke voorgrondwissel een gloednieuwe thread voor het
//! nieuwe venster en laat de oude gewoon los (`request_shutdown` + de
//! `JoinHandle` nooit joinen): een vastzittende registratie voor app A
//! blokkeert dan alleen zichzelf, nooit de events van app B die daarna
//! voorgrond wordt. De prijs is een klein, begrensd lek — een thread die
//! nooit teruggeeft blijft draaien tot het proces stopt — in ruil voor het
//! voorkomen van een permanente, procesbrede uitval.

use anyhow::{Context, Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
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

/// Lichtgewicht, samenvattend signaal van een UIA-event. `watch` bewaart
/// alleen de laatste versie: duizend events vóór de volgende capture zijn nog
/// steeds precies één reden om opnieuw te lezen, nooit duizend queued jobs.
#[derive(Clone)]
pub struct EventSignal(watch::Sender<u64>);

impl EventSignal {
    pub fn new(sender: watch::Sender<u64>) -> Self {
        Self(sender)
    }

    fn notify(&self) {
        self.0.send_modify(|version| *version = version.wrapping_add(1));
    }
}

/// Bewaakt of de STA-thread vastzit in `ElementFromHandle` of
/// `AddStructureChangedEventHandler`/`AddPropertyChangedEventHandlerNativeArray`
/// — de enige twee aanroepen op die thread die geen deadline hebben en op een
/// vastgelopen provider kunnen blijven hangen.
///
/// `0` betekent "niet bezig"; anders millis sinds `UNIX_EPOCH` waarop de
/// huidige registratie begon. Een `Arc<AtomicU64>` in plaats van een `Mutex`
/// omdat de schrijver (de STA-thread) en de lezers (`Drop`, de watchdog in
/// `read()`) nooit op elkaar hoeven te wachten — verstale of net-bijgewerkte
/// waarde lezen is hier prima, dit is geen correctheidskritisch getal.
#[derive(Clone)]
struct BusySince(Arc<AtomicU64>);

impl BusySince {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    fn enter(&self) {
        self.0.store(now_millis(), Ordering::Relaxed);
    }

    fn exit(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    /// Hoe lang de thread al onafgebroken in één registratie zit, of `None`
    /// als hij momenteel niets aan het registreren is.
    fn stuck_for(&self) -> Option<Duration> {
        let started = self.0.load(Ordering::Relaxed);
        if started == 0 {
            return None;
        }
        Some(Duration::from_millis(now_millis().saturating_sub(started)))
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// COM handlers — draaien op de STA-thread, sturen alleen platte data door.
// ---------------------------------------------------------------------------

#[implement(IUIAutomationStructureChangedEventHandler)]
struct StructureChangedHandler {
    signal: EventSignal,
}

impl IUIAutomationStructureChangedEventHandler_Impl for StructureChangedHandler_Impl {
    #[allow(non_snake_case)]
    fn HandleStructureChangedEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        _changeType: StructureChangeType,
        _runtimeId: *const SAFEARRAY,
    ) -> windows::core::Result<()> {
        self.signal.notify();
        Ok(())
    }
}

#[implement(IUIAutomationPropertyChangedEventHandler)]
struct PropertyChangedHandler {
    signal: EventSignal,
}

impl IUIAutomationPropertyChangedEventHandler_Impl for PropertyChangedHandler_Impl {
    #[allow(non_snake_case)]
    fn HandlePropertyChangedEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        _propertyId: UIA_PROPERTY_ID,
        _newValue: &VARIANT,
    ) -> windows::core::Result<()> {
        self.signal.notify();
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
    event_signal: EventSignal,
    structure_handlers: std::collections::HashMap<isize, IUIAutomationStructureChangedEventHandler>,
    property_handlers: std::collections::HashMap<isize, IUIAutomationPropertyChangedEventHandler>,
    busy: BusySince,
}

/// Markeert de thread als bezig zolang dit leeft, ongeacht of de aanroep
/// eindigt via succes, een `?`-foutretour, of een paniek.
struct BusyGuard<'a>(&'a BusySince);

impl<'a> BusyGuard<'a> {
    fn new(busy: &'a BusySince) -> Self {
        busy.enter();
        Self(busy)
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.exit();
    }
}

impl ThreadState {
    fn register(&mut self, hwnd: HWND) -> Result<()> {
        let _busy = BusyGuard::new(&self.busy);
        let hwnd_val = hwnd.0 as isize;
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        let sc_handler: IUIAutomationStructureChangedEventHandler = StructureChangedHandler {
            signal: self.event_signal.clone(),
        }
        .into();
        unsafe {
            self.automation
                .AddStructureChangedEventHandler(&root, TreeScope_Subtree, &self.cache, &sc_handler)
                .context("AddStructureChangedEventHandler faalde")?;
        }
        self.structure_handlers.insert(hwnd_val, sc_handler);

        let pc_handler: IUIAutomationPropertyChangedEventHandler = PropertyChangedHandler {
            signal: self.event_signal.clone(),
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
        let _busy = BusyGuard::new(&self.busy);
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
fn run(event_signal: EventSignal, busy: BusySince, ready: std::sync::mpsc::Sender<Result<isize>>) {
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
            event_signal,
            structure_handlers: std::collections::HashMap::new(),
            property_handlers: std::collections::HashMap::new(),
            busy,
        });
        let state_ptr = Box::into_raw(state);

        let hinstance: windows::Win32::Foundation::HINSTANCE =
            unsafe { GetModuleHandleW(None) }
                .context("modulehandvat opvragen mislukt")?
                .into();

        let class_name: Vec<u16> = "CaptureUiaEvents\0".encode_utf16().collect();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
            // De boxed state zou anders nooit meer vrijkomen.
            drop(unsafe { Box::from_raw(state_ptr) });
            return Err(anyhow!("uia-events vensterklasse registreren mislukt"));
        }
        // ERROR_CLASS_ALREADY_EXISTS is hier verwacht en onschuldig: sinds
        // elke voorgrondwissel een eigen thread start, meldt elke thread ná
        // de eerste dezelfde klasse (bij hetzelfde `hinstance`) opnieuw aan.
        // Vensterklassen zijn niet apartment- of thread-gebonden — welke
        // thread ook registreerde, `CreateWindowExW` hieronder werkt gewoon.

        let window_name: Vec<u16> = "CaptureUiaEventsWindow\0".encode_utf16().collect();
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

/// Hoeveel tijd `register`/`unregister` krijgt voordat de watchdog ze als
/// vastgelopen beschouwt. Ruim boven de 200–2500 ms die we voor een volledige
/// boomlezing gemeten hebben (`reader.rs`) — deze twee doen veel minder werk
/// (geen boomwandeling), dus dit is al een royale marge.
const STUCK_THRESHOLD: Duration = Duration::from_secs(5);
/// Hoeveel registratie-threads hoogstens tegelijk mogen vastzitten. Daarna
/// schakelen we UIA-events tijdelijk uit voor nieuwe vensters; polling blijft
/// actief en er ontstaan geen onbeperkte threads.
const MAX_ABANDONED_THREADS: usize = 2;

/// Eén draaiende message-only thread, toegewijd aan precies één venster voor
/// de rest van zijn leven. `hwnd` is het adres van *deze* thread om naartoe
/// te posten, niet het doelvenster (dat kreeg zijn registratie al bij het
/// aanmaken, zie `spawn_window_thread`).
struct WindowThread {
    hwnd: isize,
    busy: BusySince,
    /// Voorkomt dat een vastgelopen thread bij elke tik opnieuw gelogd wordt.
    warned_stuck: std::sync::atomic::AtomicBool,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl WindowThread {
    /// Meldt eenmalig — niet bij elke tik opnieuw — of deze thread al langer
    /// dan `STUCK_THRESHOLD` in zijn registratie vastzit. `PostMessageW`
    /// blijft daarna gewoon werken (die blokkeert nooit op een volle
    /// wachtrij bij dit soort volumes); dit is puur zichtbaarheid voor een
    /// situatie die anders geruisloos verdwijnt in "er komen geen events
    /// meer" zonder dat iets zegt waarom.
    fn warn_if_stuck(&self, app_key: &str) {
        match self.busy.stuck_for() {
            Some(elapsed) if elapsed >= STUCK_THRESHOLD => {
                if !self.warned_stuck.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        app = app_key,
                        seconden = elapsed.as_secs(),
                        "uia-eventthread zit vast in een registratie bij een provider; \
                         events blijven uit, polling werkt gewoon door"
                    );
                }
            }
            Some(_) => {}
            None => self.warned_stuck.store(false, Ordering::Relaxed),
        }
    }

    fn request_shutdown(&self) {
        let target = HWND(self.hwnd as *mut core::ffi::c_void);
        unsafe {
            let _ = PostMessageW(Some(target), WM_UIA_SHUTDOWN, WPARAM(0), LPARAM(0));
        }
    }
}

/// Vraagt afsluiten aan en ruimt de join-handle op een reaper-thread op. Een
/// werkelijk vastgelopen COM-aanroep kan zo nooit de pipeline blokkeren; het
/// aantal van zulke gevallen wordt tegelijk begrensd door `switch_window`.
fn retire_window_thread(mut thread: WindowThread, abandoned: Arc<AtomicUsize>) {
    let was_stuck = thread.busy.stuck_for().is_some();
    thread.request_shutdown();
    let Some(handle) = thread.join_handle.take() else {
        return;
    };
    if was_stuck {
        abandoned.fetch_add(1, Ordering::Relaxed);
    }
    std::thread::spawn(move || {
        let _ = handle.join();
        if was_stuck {
            abandoned.fetch_sub(1, Ordering::Relaxed);
        }
    });
}

/// Start een nieuwe thread die zijn eigen `IUIAutomation` opbouwt en, zodra
/// zijn message-only venster bestaat, een registratie voor `target` post
/// (fire-and-forget: deze functie wacht nooit op die registratie zelf, alleen
/// op het — snelle, deadline-loze-COM-aanroep-vrije — aanmaken van het
/// venster). `target = None` is de eenmalige proefopstart in `start()`: die
/// bevestigt dat COM/STA hier werkt, zonder iets te registreren.
fn spawn_window_thread(event_signal: EventSignal, target: Option<HWND>) -> Result<WindowThread> {
    let target_val = target.map(|h| h.0 as isize);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize>>();
    let busy = BusySince::new();
    let busy_for_thread = busy.clone();

    let join_handle = std::thread::Builder::new()
        .name("capture-uia-events".into())
        .spawn(move || run(event_signal, busy_for_thread, ready_tx))
        .context("uia-events thread starten mislukt")?;

    let hwnd = ready_rx
        .recv()
        .map_err(|_| anyhow!("uia-events thread startte niet"))??;

    if let Some(target_val) = target_val {
        let msg_window = HWND(hwnd as *mut core::ffi::c_void);
        unsafe {
            let _ = PostMessageW(Some(msg_window), WM_UIA_REGISTER, WPARAM(0), LPARAM(target_val));
        }
    }

    Ok(WindowThread {
        hwnd,
        busy,
        warned_stuck: std::sync::atomic::AtomicBool::new(false),
        join_handle: Some(join_handle),
    })
}

/// Handvat voor het event-driven UIA-mechanisme. Houdt hoogstens één
/// `WindowThread` vast — die van het huidige voorgrondvenster — achter een
/// `Mutex` omdat `switch_window`/`warn_if_stuck` via `&self` aangeroepen
/// worden. Zie de moduledocumentatie bovenaan voor waarom dit per venster een
/// eigen thread is in plaats van één gedeelde.
pub struct UiaEventThread {
    event_signal: EventSignal,
    current: std::sync::Mutex<Option<WindowThread>>,
    abandoned: Arc<AtomicUsize>,
}

impl UiaEventThread {
    pub fn start(event_sender: watch::Sender<u64>) -> Result<Self> {
        let event_signal = EventSignal::new(event_sender);
        // Eenmalige proefopstart: bevestigt dat COM/STA en de vensterklasse
        // op dit systeem werken. Lukt dit niet, dan valt de aanroeper terug
        // op alleen polling — precies zoals vóór deze aanpak, alleen faalt
        // dat nu één keer bij start in plaats van stilzwijgend bij elke
        // voorgrondwissel opnieuw. Lukt het wel, dan gooien we deze
        // wegwerp-thread meteen weg; pas de eerste `switch_window` start de
        // thread die er echt toe doet.
        let probe = spawn_window_thread(event_signal.clone(), None)?;
        probe.request_shutdown();

        Ok(Self {
            event_signal,
            current: std::sync::Mutex::new(None),
            abandoned: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Registreert events voor `hwnd` op een gloednieuwe thread en laat de
    /// vorige los. Blokkeert nooit op de registratie zelf (die gebeurt async
    /// op de nieuwe thread) en nooit op een eventueel vastzittende oude
    /// thread — die krijgt een shutdown-bericht en wordt dan losgelaten
    /// zonder erop te wachten (`join_handle` wordt nooit gejoind), zodat een
    /// hangende registratie voor het vorige venster nooit de events van dit
    /// venster tegenhoudt.
    pub fn switch_window(&self, hwnd: HWND) -> Result<()> {
        // Een provider kan in registratie blijven hangen. Laat hoogstens één
        // al-vastgelopen huidige thread plaatsmaken wanneer er al één oude
        // hangt; daarna valt deze optimalisatie terug op polling in plaats van
        // bij elke focuswissel nog een thread te verliezen.
        let current_is_stuck = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|thread| thread.busy.stuck_for().is_some());
        if current_is_stuck
            && self.abandoned.load(Ordering::Relaxed) >= MAX_ABANDONED_THREADS - 1
        {
            return Err(anyhow!(
                "te veel vastgelopen UIA-eventregistraties; terugvallen op polling"
            ));
        }

        let new = spawn_window_thread(self.event_signal.clone(), Some(hwnd))?;
        let old = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(new);
        if let Some(old) = old {
            retire_window_thread(old, Arc::clone(&self.abandoned));
        }
        Ok(())
    }

    /// Meldt of de thread van het huidige venster vastzit in zijn
    /// registratie. Zie `WindowThread::warn_if_stuck`.
    pub fn warn_if_stuck(&self, app_key: &str) {
        let guard = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wt) = guard.as_ref() {
            wt.warn_if_stuck(app_key);
        }
    }
}

impl Drop for UiaEventThread {
    fn drop(&mut self) {
        let Some(current) = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        else {
            return;
        };
        retire_window_thread(current, Arc::clone(&self.abandoned));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_signaal_coalescet_wijzigingen() {
        let (sender, receiver) = watch::channel(0);
        let signal = EventSignal::new(sender);
        signal.notify();
        signal.notify();
        signal.notify();
        assert_eq!(*receiver.borrow(), 3);
    }

    // Deze vier testen de horlogelogica zonder COM of een echte thread nodig
    // te hebben — precies het deel dat we niet betrouwbaar kunnen bewijzen
    // door een echte provider te laten vasthangen.

    #[test]
    fn busysince_begint_leeg() {
        assert_eq!(BusySince::new().stuck_for(), None);
    }

    #[test]
    fn busysince_rapporteert_bezig_na_enter() {
        let busy = BusySince::new();
        busy.enter();
        let elapsed = busy.stuck_for().expect("moet bezig zijn na enter()");
        assert!(elapsed < Duration::from_secs(1), "was {elapsed:?}");
    }

    #[test]
    fn busysince_is_weer_leeg_na_exit() {
        let busy = BusySince::new();
        busy.enter();
        busy.exit();
        assert_eq!(busy.stuck_for(), None);
    }

    #[test]
    fn busyguard_ruimt_op_bij_normale_return_en_bij_paniek() {
        let busy = BusySince::new();

        {
            let _guard = BusyGuard::new(&busy);
            assert!(busy.stuck_for().is_some(), "guard moet enter() aanroepen");
        }
        assert_eq!(
            busy.stuck_for(),
            None,
            "guard moet exit() aanroepen bij normale drop"
        );

        let busy2 = BusySince::new();
        let busy2_ref = &busy2;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = BusyGuard::new(busy2_ref);
            panic!("gesimuleerde vastgelopen COM-aanroep");
        }));
        assert!(result.is_err());
        assert_eq!(
            busy2.stuck_for(),
            None,
            "guard moet ook opruimen als de aanroep paniekt"
        );
    }
}
