use super::macos_mediaremote::NowPlaying;
use super::{Media, PlatformCapabilities};
use auto_launch::{AutoLaunch, MacOSLaunchMode};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

const APP_NAME: &str = "ggoled_app";
const APP_BUNDLE_IDENTIFIER: &str = "com.ggoled.app";

static MEDIA_INIT_LOGGED: AtomicBool = AtomicBool::new(false);
static MEDIA_READ_LOGGED: AtomicBool = AtomicBool::new(false);
static IDLE_LOGGED: AtomicBool = AtomicBool::new(false);
static AUTOSTART_INIT_LOGGED: AtomicBool = AtomicBool::new(false);
static AUTOSTART_SET_LOGGED: AtomicBool = AtomicBool::new(false);
static AUTOSTART_GET_LOGGED: AtomicBool = AtomicBool::new(false);

fn log_once(flag: &AtomicBool, msg: impl AsRef<str>) {
    if flag
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        eprintln!("{}", msg.as_ref());
    }
}

fn media_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("GGOLED_MEDIAREMOTE_DEBUG") {
        Ok(value) => {
            let lower = value.trim().to_ascii_lowercase();
            !(lower.is_empty() || lower == "0" || lower == "false" || lower == "off")
        }
        Err(_) => false,
    })
}

fn media_debug(msg: impl AsRef<str>) {
    if media_debug_enabled() {
        eprintln!("[media] {}", msg.as_ref());
    }
}

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        media: true,
        idle_timeout: true,
        autostart: true,
    }
}

pub struct MediaControl {
    now_playing: Option<NowPlaying>,
}

impl MediaControl {
    pub fn new() -> MediaControl {
        media_debug("MediaControl::new");
        let now_playing = catch_unwind(NowPlaying::new).ok();
        if now_playing.is_none() {
            log_once(
                &MEDIA_INIT_LOGGED,
                "failed to initialize MediaRemote; media metadata will be unavailable",
            );
            media_debug("NowPlaying::new failed (panic)");
        }
        MediaControl { now_playing }
    }

    pub fn get_media(&self, include_paused: bool) -> Option<Media> {
        let now_playing = match self.now_playing.as_ref() {
            Some(now_playing) => now_playing,
            None => {
                media_debug("get_media: now_playing unavailable");
                return None;
            }
        };
        let info_guard = match catch_unwind(AssertUnwindSafe(|| now_playing.get_info())) {
            Ok(guard) => guard,
            Err(_) => {
                log_once(
                    &MEDIA_READ_LOGGED,
                    "failed to query MediaRemote state; media metadata will be unavailable",
                );
                media_debug("get_media: now_playing.get_info panicked");
                return None;
            }
        };
        let info = &*info_guard;
        media_debug(format!(
            "get_media snapshot: is_playing={:?}, title={:?}, artist={:?}",
            info.is_playing, info.title, info.artist
        ));
        if !include_paused && !info.is_playing.unwrap_or(false) {
            media_debug("get_media: filtered because is_playing != true");
            return None;
        }
        let title = match info.title.as_ref() {
            Some(title) => title.trim(),
            None => {
                media_debug("get_media: filtered because title missing");
                return None;
            }
        };
        let artist = match info.artist.as_ref() {
            Some(artist) => artist.trim(),
            None => {
                media_debug("get_media: filtered because artist missing");
                return None;
            }
        };
        if title.is_empty() || artist.is_empty() {
            media_debug("get_media: filtered because title/artist empty");
            return None;
        }
        media_debug(format!(
            "get_media: returning media title={:?} artist={:?}",
            title, artist
        ));
        Some(Media {
            title: title.to_string(),
            artist: artist.to_string(),
        })
    }
}

pub fn get_idle_seconds() -> usize {
    match system_idle_time::get_idle_time() {
        Ok(idle) => idle.as_secs() as usize,
        Err(err) => {
            log_once(&IDLE_LOGGED, format!("failed to query macOS idle time: {err}"));
            0
        }
    }
}

fn autolaunch() -> Option<AutoLaunch> {
    let exe = match std::env::current_exe() {
        Ok(path) => path.canonicalize().unwrap_or(path),
        Err(err) => {
            log_once(
                &AUTOSTART_INIT_LOGGED,
                format!("failed to get current executable path: {err}"),
            );
            return None;
        }
    };
    let exe_path = exe.to_string_lossy().to_string();
    Some(AutoLaunch::new(
        APP_NAME,
        &exe_path,
        MacOSLaunchMode::LaunchAgent,
        &[] as &[&str],
        &[APP_BUNDLE_IDENTIFIER],
        "",
    ))
}

pub fn set_autostart(enabled: bool) {
    let Some(autolaunch) = autolaunch() else {
        return;
    };
    let result = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    if let Err(err) = result {
        log_once(
            &AUTOSTART_SET_LOGGED,
            format!("failed to set macOS autostart to {enabled}: {err}"),
        );
    }
}

pub fn get_autostart() -> bool {
    let Some(autolaunch) = autolaunch() else {
        return false;
    };
    match autolaunch.is_enabled() {
        Ok(enabled) => enabled,
        Err(err) => {
            log_once(
                &AUTOSTART_GET_LOGGED,
                format!("failed to query macOS autostart state: {err}"),
            );
            false
        }
    }
}
