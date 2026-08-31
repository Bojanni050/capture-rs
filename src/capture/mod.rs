pub mod events;
pub mod idle;
pub mod screen;
pub mod window;

pub use events::ForegroundEvents;
pub use idle::idle_seconds;
pub use screen::ScreenCapturer;
pub use window::{
    foreground, foreground_hwnd, top_level_windows, window_rect, Rect, WindowInfo,
};
