//! Systemtray-icoon: een gekleurde stip die laat zien of de opname draait
//! (groen), of je even weg bent (geel) of dat de laatste tik mislukte
//! (rood), met een rechtsklik-menu voor het dashboard, automatisch
//! opstarten en afsluiten.
//!
//! Draait op zijn eigen OS-thread. `Shell_NotifyIcon`-berichten komen bij
//! een verborgen venster binnen dat de `tray-icon`-crate zelf aanmaakt, en
//! Windows levert die alleen af als de thread die het venster aanmaakte ook
//! zijn eigen berichtenlus draait — vandaar de `PeekMessageW`-lus hieronder
//! in plaats van dit aan tokio over te laten.

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

const ICON_SIZE: u32 = 32;

/// Zichtbare staat van de opname.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Recording,
    Idle,
    Error,
}

/// Gedeeld met de opnamelus: die zet de staat na elke tik, de tray-thread
/// leest 'm op zijn eigen tempo. Geen callback nodig — een atomic is genoeg
/// voor drie waarden die alleen "de laatste stand" hoeven te zijn.
#[derive(Clone)]
pub struct TrayStatus(Arc<AtomicU8>);

impl TrayStatus {
    fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }

    pub fn set(&self, status: Status) {
        self.0.store(status as u8, Ordering::Relaxed);
    }

    fn get(&self) -> Status {
        match self.0.load(Ordering::Relaxed) {
            1 => Status::Idle,
            2 => Status::Error,
            _ => Status::Recording,
        }
    }
}

/// Start het icoon op een eigen thread en geeft meteen de handle terug
/// waarmee de opnamelus de status bijwerkt. Mislukt het aanmaken zelf (geen
/// desktop-sessie, geen Explorer-shell) dan gaat de opname gewoon door — het
/// icoon is nooit een voorwaarde voor het vastleggen.
pub fn spawn(dashboard_url: String, shutdown: tokio::sync::watch::Sender<bool>) -> TrayStatus {
    let status = TrayStatus::new();
    let status_for_thread = status.clone();
    let spawned = std::thread::Builder::new()
        .name("capture-tray".into())
        .spawn(move || {
            if let Err(e) = run(dashboard_url, shutdown, status_for_thread) {
                tracing::warn!(error = %e, "systemtray-icoon gestopt met een fout");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "kon systemtray-thread niet starten");
    }
    status
}

fn run(url: String, shutdown: tokio::sync::watch::Sender<bool>, status: TrayStatus) -> Result<()> {
    let menu = Menu::new();
    let open_item = MenuItem::new("Open dashboard", true, None);
    let autostart_item = CheckMenuItem::new(
        "Automatisch opstarten bij inloggen",
        true,
        crate::autostart::is_enabled(),
        None,
    );
    let quit_item = MenuItem::new("Capture afsluiten", true, None);
    menu.append(&open_item).context("menu opbouwen mislukt")?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&autostart_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let mut current = Status::Recording;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip_for(current))
        .with_icon(dot_icon(current))
        .build()
        .context("systemtray-icoon aanmaken mislukt")?;

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();

    loop {
        pump_messages();

        if let Ok(TrayIconEvent::DoubleClick { .. }) = tray_rx.try_recv() {
            open_dashboard(&url);
        }

        if let Ok(event) = menu_rx.try_recv() {
            if &event.id == open_item.id() {
                open_dashboard(&url);
            } else if &event.id == autostart_item.id() {
                let wanted = autostart_item.is_checked();
                let result = if wanted {
                    crate::autostart::enable()
                } else {
                    crate::autostart::disable()
                };
                if let Err(e) = result {
                    tracing::warn!(error = %e, "automatisch opstarten aanpassen mislukt");
                    // Terugdraaien: het vinkje moet de echte registerstatus blijven volgen.
                    autostart_item.set_checked(!wanted);
                }
            } else if &event.id == quit_item.id() {
                let _ = shutdown.send(true);
                break;
            }
        }

        let wanted = status.get();
        if wanted != current {
            current = wanted;
            let _ = tray.set_icon(Some(dot_icon(current)));
            let _ = tray.set_tooltip(Some(tooltip_for(current)));
        }

        std::thread::sleep(Duration::from_millis(150));
    }

    drop(tray);
    Ok(())
}

/// Verwerkt openstaande vensterberichten zonder te blokkeren — nodig zodat
/// het verborgen venster van de tray-crate klikken kan afleveren, maar de
/// lus hierboven ook zonder berichten op zijn eigen tempo blijft draaien.
fn pump_messages() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn open_dashboard(url: &str) {
    if let Err(e) = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
    {
        tracing::warn!(error = %e, "dashboard openen mislukt");
    }
}

fn tooltip_for(status: Status) -> &'static str {
    match status {
        Status::Recording => "Capture — opname actief",
        Status::Idle => "Capture — je bent even weg",
        Status::Error => "Capture — laatste tik mislukt",
    }
}

/// Tekent een gevulde, licht antialiased cirkel in de statuskleur. Geen los
/// .ico-bestand nodig: het icoon wordt in het geheugen opgebouwd uit ruwe
/// RGBA-pixels, met dezelfde `image`-crate die de rest van Capture al
/// gebruikt voor screenshots.
fn dot_icon(status: Status) -> Icon {
    let color = match status {
        Status::Recording => [34u8, 197, 94, 255],
        Status::Idle => [234, 179, 8, 255],
        Status::Error => [239, 68, 68, 255],
    };

    let size = ICON_SIZE;
    let center = (size - 1) as f32 / 2.0;
    let radius = center - 1.0;
    let mut img = RgbaImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let edge = dist - radius;
            let alpha = if edge <= -1.0 {
                255u8
            } else if edge >= 1.0 {
                0
            } else {
                (((1.0 - edge) / 2.0).clamp(0.0, 1.0) * 255.0) as u8
            };
            img.put_pixel(x, y, Rgba([color[0], color[1], color[2], alpha]));
        }
    }
    Icon::from_rgba(img.into_raw(), size, size).expect("32x32 RGBA-icoon is altijd geldig")
}
