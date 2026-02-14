#[derive(PartialEq)]
pub struct Media {
    pub title: String,
    pub artist: String,
}

#[derive(Clone, Copy)]
pub struct PlatformCapabilities {
    pub media: bool,
    pub idle_timeout: bool,
    pub autostart: bool,
}

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "macos")]
mod macos_mediaremote;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        media: false,
        idle_timeout: false,
        autostart: false,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub fn set_autostart(_enabled: bool) {}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub fn get_autostart() -> bool {
    false
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub struct MediaControl;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
impl MediaControl {
    pub fn new() -> Self {
        Self
    }

    pub fn get_media(&self, _include_paused: bool) -> Option<Media> {
        None
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub fn get_idle_seconds() -> usize {
    0
}
