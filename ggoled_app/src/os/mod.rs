use draconis::{init_static_plugins, initialize_plugin_manager, shutdown_plugin_manager, CacheManager, Plugin};

#[derive(PartialEq)]
pub struct Media {
    pub title: String,
    pub artist: String,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
pub enum VolumeKeySignal {
    Up,
    Down,
    Mute,
}

#[derive(Clone, Copy)]
pub struct PlatformCapabilities {
    pub media: bool,
    pub idle_timeout: bool,
    pub autostart: bool,
}

pub struct MediaControl {
    plugin: Option<Plugin>,
    cache: CacheManager,
}

impl MediaControl {
    pub fn new() -> MediaControl {
        initialize_plugin_manager();
        let count = init_static_plugins();
        if count == 0 {
            tracing::warn!("No static plugins registered");
        }

        let mut cache = CacheManager::new();
        let mut plugin = Plugin::new("NowPlayingPlugin").ok();

        if let Some(ref mut p) = plugin {
            if let Err(e) = p.initialize(&mut cache) {
                tracing::warn!("Failed to initialize NowPlayingPlugin: {:?}", e);
                plugin = None;
            }
        }

        if plugin.is_none() {
            tracing::warn!("Failed to load NowPlayingPlugin");
        }

        MediaControl { plugin, cache }
    }

    pub fn get_media(&mut self, _include_paused: bool) -> Option<Media> {
        let plugin = self.plugin.as_mut()?;

        if let Err(e) = plugin.collect_data(&mut self.cache) {
            let last_error = plugin.get_last_error();
            tracing::warn!("Failed to collect plugin data: {:?} (last_error: {:?})", e, last_error);
            return None;
        }

        let fields = plugin.get_fields().ok()?;

        tracing::debug!("Plugin fields: {:?}", fields);

        let title = fields.get("title")?.clone();
        let artist = fields.get("artist").cloned().unwrap_or_default();

        if title.is_empty() {
            return None;
        }

        Some(Media { title, artist })
    }
}

impl Drop for MediaControl {
    fn drop(&mut self) {
        shutdown_plugin_manager();
    }
}

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        media: true,
        idle_timeout: false,
        autostart: false,
    }
}

pub fn set_autostart(_enabled: bool) {}
pub fn get_autostart() -> bool {
    false
}
pub fn get_idle_seconds() -> usize {
    0
}

#[cfg(target_os = "macos")]
pub fn start_volume_key_listener() -> Option<std::sync::mpsc::Receiver<VolumeKeySignal>> {
    None
}

#[cfg(target_os = "macos")]
pub fn ensure_accessibility_permission(_prompt: bool) -> bool {
    true
}
