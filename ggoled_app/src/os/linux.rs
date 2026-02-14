use super::{Media, PlatformCapabilities};
use mpris::{PlaybackStatus, PlayerFinder};

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        media: true,
        idle_timeout: false,
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
                eprintln!("failed to create MPRIS player finder: {err:?}");
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
    // TODO
    0
}

pub fn set_autostart(_enabled: bool) {}

pub fn get_autostart() -> bool {
    false
}
