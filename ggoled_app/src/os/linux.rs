use super::{Media, PlatformCapabilities};
use mpris::{PlaybackStatus, PlayerFinder};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::warn;

static IDLE_LOGGED: AtomicBool = AtomicBool::new(false);

pub fn capabilities() -> PlatformCapabilities {
    let idle_timeout = system_idle_time::get_idle_time().is_ok();
    if !idle_timeout
        && IDLE_LOGGED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        warn!("failed to query Linux idle time; idle-timeout will be disabled");
    }
    PlatformCapabilities {
        media: true,
        idle_timeout,
        autostart: false,
    }
}

pub struct MediaControl {
    pf: Option<PlayerFinder>,
}
impl MediaControl {
    pub fn new() -> MediaControl {
        let pf = match PlayerFinder::new() {
            Ok(pf) => Some(pf),
            Err(err) => {
                warn!(?err, "failed to create MPRIS player finder");
                None
            }
        };
        MediaControl { pf }
    }
    pub fn get_media(&self, include_paused: bool) -> Option<Media> {
        let pf = self.pf.as_ref()?;
        let player = pf.find_active().ok()?;
        let status = player.get_playback_status().ok()?;
        let allowed =
            matches!(status, PlaybackStatus::Playing) || (include_paused && matches!(status, PlaybackStatus::Paused));
        if !allowed {
            return None;
        }
        let meta = player.get_metadata().ok()?;
        let artists = meta.artists()?;
        let artist = artists.first()?;
        let title = meta.title()?;
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
            if IDLE_LOGGED
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                warn!(?err, "failed to query Linux idle time");
            }
            0
        }
    }
}

pub fn set_autostart(_enabled: bool) {}

pub fn get_autostart() -> bool {
    false
}
