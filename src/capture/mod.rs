pub mod events;
pub mod idle;
pub mod screen;
pub mod window;

pub use events::ForegroundEvents;
pub use idle::idle_seconds;
pub use screen::{ScreenCapturer, Shot};
pub use window::{
    Rect, WindowInfo, foreground, foreground_hwnd, is_shell_window, stacked_windows,
    top_level_windows, window_rect,
};
