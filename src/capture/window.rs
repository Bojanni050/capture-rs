//! Welk venster staat er op de voorgrond, en van welk proces is het?
//!
//! Dit is de goedkoopste context die we hebben: het kost microseconden en
//! bepaalt of we überhaupt een screenshot hoeven te maken.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM};
use windows::core::BOOL;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::PWSTR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Volledige venstertitel.
    pub title: String,
    /// Bestandsnaam van het proces in kleine letters, bv. "chrome.exe".
    pub exe: String,
    /// Volledig pad, handig voor `doctor` en debuggen.
    pub exe_path: String,
    pub pid: u32,
    /// Het vensterhandvat als getal. `HWND` is niet `Send`, en UI Automation
    /// leest het venster op een andere thread dan waar we het ophaalden.
    pub hwnd: isize,
}

impl WindowInfo {
    /// Naam zonder extensie, wat we in de UI en denylist gebruiken.
    pub fn app_key(&self) -> String {
        self.exe
            .strip_suffix(".exe")
            .unwrap_or(&self.exe)
            .to_string()
    }
}

/// Het actieve venster, of `None` als er geen bruikbare voorgrond is
/// (vergrendeld scherm, alt-tab-overlay, net van bureaublad gewisseld).
pub fn foreground() -> Option<WindowInfo> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        let title = if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let (exe_path, exe) = process_image(pid).unwrap_or_default();

        Some(WindowInfo {
            title,
            exe,
            exe_path,
            pid,
            hwnd: hwnd.0 as isize,
        })
    }
}

/// Alle zichtbare vensters met een titel.
///
/// Gebruikt door `doctor` om te laten zien welke van jouw apps bruikbare
/// accessibility-informatie geven, zonder dat je ze stuk voor stuk naar de
/// voorgrond hoeft te halen.
pub fn top_level_windows() -> Vec<WindowInfo> {
    let mut found: Vec<WindowInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut found as *mut Vec<WindowInfo> as isize),
        );
    }
    found
}

/// Callback voor `EnumWindows`; `lparam` wijst naar de verzamel-Vec.
unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };

    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return true.into();
    }

    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        return true.into();
    }
    let title = String::from_utf16_lossy(&buf[..len as usize]);

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return true.into();
    }

    let (exe_path, exe) = process_image(pid).unwrap_or_default();
    out.push(WindowInfo {
        title,
        exe,
        exe_path,
        pid,
        hwnd: hwnd.0 as isize,
    });

    true.into()
}

/// (volledig pad, bestandsnaam in kleine letters) van een proces.
fn process_image(pid: u32) -> Option<(String, String)> {
    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        result.ok()?;

        let full = String::from_utf16_lossy(&buf[..size as usize]);
        let file = full
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&full)
            .to_lowercase();
        Some((full, file))
    }
}
