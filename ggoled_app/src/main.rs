#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod os;

use chrono::{DateTime, Local, TimeDelta, Timelike};
use ggoled_draw::{
    bitmap_from_memory, DrawDevice, DrawEvent, DrawLayer, LayerId, ShiftMode, TextOverflowMode, TextRenderer,
};
use ggoled_lib::Device;
use os::{capabilities, get_autostart, get_idle_seconds, set_autostart, Media, MediaControl, PlatformCapabilities};
#[cfg(target_os = "macos")]
use os::{ensure_accessibility_permission, start_volume_key_listener, VolumeKeySignal};
use rfd::{MessageDialog, MessageLevel};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tracing::{debug, info, warn};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, Submenu},
    Icon as TrayIconImage, TrayIcon, TrayIconBuilder,
};

const IDLE_TIMEOUT_SECS: usize = 60;
const NOTIF_DUR: Duration = Duration::from_secs(5);
const TICK_DUR_FAST: Duration = Duration::from_millis(10);
const TICK_DUR_NORMAL: Duration = Duration::from_millis(250);
const BASE_STATION_VOLUME_MAX: u8 = 56;
const BASE_STATION_VOLUME_STEP: u8 = 4;
const NOTIF_MARGIN_X: isize = 0;
const NOTIF_MARGIN_Y: isize = 0;
const OLED_WIDTH: isize = 128;
const VOLUME_ICON_GAP: isize = 1;

fn volume_keys_debug(msg: impl AsRef<str>) {
    debug!(target: "volume-keys", "{}", msg.as_ref());
}

fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("ggoled_app=info,volume-keys=debug,media=info,mediaremote=info")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true)
        .try_init();
}

#[derive(Serialize, Deserialize, Default, Clone, Copy)]
enum ConfigShiftMode {
    Off,
    #[default]
    Simple,
}
impl ConfigShiftMode {
    fn to_api(self) -> ShiftMode {
        match self {
            ConfigShiftMode::Off => ShiftMode::Off,
            ConfigShiftMode::Simple => ShiftMode::Simple,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct ConfigFont {
    path: PathBuf,
    size: f32,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config {
    font: Option<ConfigFont>,
    show_time: bool,
    show_media: bool,
    show_media_paused: bool,
    idle_timeout: bool,
    oled_shift: ConfigShiftMode,
    show_notifications: bool,
    autostart: bool,
    pass_through_volume_keys: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            font: None,
            show_time: true,
            show_media: true,
            show_media_paused: false,
            idle_timeout: true,
            oled_shift: ConfigShiftMode::default(),
            show_notifications: true,
            autostart: false,
            pass_through_volume_keys: false,
        }
    }
}
impl Config {
    fn path() -> PathBuf {
        directories::BaseDirs::new()
            .unwrap()
            .config_dir()
            .join("ggoled_app.toml")
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let text = toml::to_string(self)?;
        std::fs::write(Self::path(), text)?;
        Ok(())
    }
    pub fn load() -> Config {
        let Ok(text) = std::fs::read_to_string(Self::path()) else {
            return Config::default();
        };
        let Ok(conf) = toml::from_str(&text) else {
            return Config::default();
        };
        conf
    }
}

fn show_error_dialog(msg: &str) {
    MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title("ggoled")
        .set_description(msg)
        .show();
}

fn volume_from_percent(percent: u8) -> u8 {
    (((percent as u16) * (BASE_STATION_VOLUME_MAX as u16) + 50) / 100) as u8
}

fn volume_to_percent(volume: u8) -> u8 {
    (((volume.min(BASE_STATION_VOLUME_MAX) as u16) * 100 + (BASE_STATION_VOLUME_MAX as u16 / 2))
        / BASE_STATION_VOLUME_MAX as u16) as u8
}

fn volume_icon_level(percent: u8) -> usize {
    if percent == 0 {
        0 // muted: speaker + X
    } else if percent <= 32 {
        1 // no waves
    } else if percent <= 64 {
        2 // one wave
    } else if percent >= 100 {
        4 // three waves
    } else {
        3 // two waves
    }
}

fn make_volume_icon(waves: u8, muted: bool) -> ggoled_lib::Bitmap {
    let mut icon = ggoled_lib::Bitmap::new(12, 9, false);
    let points = vec![
        // Speaker body + cone (always shown).
        (0, 3),
        (0, 4),
        (0, 5),
        (1, 2),
        (1, 3),
        (1, 4),
        (1, 5),
        (1, 6),
        (2, 2),
        (2, 3),
        (2, 4),
        (2, 5),
        (2, 6),
        (3, 1),
        (3, 2),
        (3, 3),
        (3, 4),
        (3, 5),
        (3, 6),
        (3, 7),
    ];
    let wave_1 = vec![(5, 2), (6, 3), (6, 4), (6, 5), (5, 6)];
    let wave_2 = vec![(7, 1), (8, 2), (8, 3), (8, 4), (8, 5), (8, 6), (7, 7)];
    let wave_3 = vec![
        (9, 0),
        (10, 1),
        (11, 2),
        (11, 3),
        (11, 4),
        (11, 5),
        (11, 6),
        (10, 7),
        (9, 8),
    ];

    let mut points = points;
    if waves >= 1 {
        points.extend(wave_1);
    }
    if waves >= 2 {
        points.extend(wave_2);
    }
    if waves >= 3 {
        points.extend(wave_3);
    }
    if muted {
        // Mute marker X on the right side.
        points.extend([(7, 2), (8, 3), (9, 4), (10, 5), (10, 2), (9, 3), (8, 4), (7, 5)]);
    }

    for (x, y) in points {
        icon.data.set(y * icon.w + x, true);
    }
    icon
}

struct TrayState {
    tray: TrayIcon,
    icon_ok: TrayIconImage,
    icon_error: TrayIconImage,
    tm_time_check: CheckMenuItem,
    tm_media_check: CheckMenuItem,
    tm_media_paused_check: CheckMenuItem,
    tm_notif_check: CheckMenuItem,
    tm_idle_check: CheckMenuItem,
    tm_autostart_check: CheckMenuItem,
    tm_volume_down: MenuItem,
    tm_volume_up: MenuItem,
    tm_volume_mute: MenuItem,
    tm_volume_25: MenuItem,
    tm_volume_50: MenuItem,
    tm_volume_75: MenuItem,
    tm_volume_100: MenuItem,
    #[cfg(target_os = "macos")]
    tm_pass_through_volume_keys_check: CheckMenuItem,
    tm_shift_off: CheckMenuItem,
    tm_shift_simple: CheckMenuItem,
    tm_quit: MenuItem,
}

struct RuntimeState {
    capabilities: PlatformCapabilities,
    config: Config,
    tray: TrayState,
    dev: DrawDevice,
    mgr: MediaControl,
    last_time: DateTime<Local>,
    last_media: Option<Media>,
    time_layers: Vec<LayerId>,
    media_layers: Vec<LayerId>,
    notif_layers: Vec<LayerId>,
    notif_expiry: DateTime<Local>,
    is_connected: Option<bool>,
    volume: Option<u8>,
    needs_redraw: bool,
    icon_hs_connect: Arc<ggoled_lib::Bitmap>,
    icon_hs_disconnect: Arc<ggoled_lib::Bitmap>,
    icon_volume_levels: [Arc<ggoled_lib::Bitmap>; 5],
    #[cfg(target_os = "macos")]
    volume_key_rx: Option<std::sync::mpsc::Receiver<VolumeKeySignal>>,
}

enum UserEvent {
    MenuEvent(MenuEvent),
    ShutdownRequested,
}

impl RuntimeState {
    fn new(config: Config, capabilities: PlatformCapabilities, tray: TrayState) -> anyhow::Result<RuntimeState> {
        #[allow(unused_mut)]
        let mut config = config;
        #[allow(unused_mut)]
        let mut tray = tray;

        let icon_hs_connect =
            Arc::new(bitmap_from_memory(include_bytes!("../assets/headset_connected.png"), 0x80).unwrap());
        let icon_hs_disconnect =
            Arc::new(bitmap_from_memory(include_bytes!("../assets/headset_disconnected.png"), 0x80).unwrap());
        let icon_volume_levels = [
            Arc::new(make_volume_icon(0, true)),
            Arc::new(make_volume_icon(0, false)),
            Arc::new(make_volume_icon(1, false)),
            Arc::new(make_volume_icon(2, false)),
            Arc::new(make_volume_icon(3, false)),
        ];

        let mut dev = DrawDevice::new(Device::connect()?, 30);
        if let Some(font) = &config.font {
            dev.texter = TextRenderer::load_from_file(&font.path, font.size)?;
        }

        dev.set_shift_mode(config.oled_shift.to_api());
        dev.play();

        #[cfg(target_os = "macos")]
        let volume_key_rx = if config.pass_through_volume_keys {
            volume_keys_debug("startup: passthrough enabled in config; checking accessibility permission");
            if !ensure_accessibility_permission(false) {
                config.pass_through_volume_keys = false;
                tray.tm_pass_through_volume_keys_check.set_checked(false);
                warn!("accessibility permission missing for media volume key passthrough");
                volume_keys_debug("startup: permission missing; disabling passthrough");
                None
            } else {
                volume_keys_debug("startup: permission granted; starting key listener");
                let rx = start_volume_key_listener();
                if rx.is_none() {
                    config.pass_through_volume_keys = false;
                    tray.tm_pass_through_volume_keys_check.set_checked(false);
                    warn!("failed to start media volume key passthrough listener");
                    volume_keys_debug("startup: listener start failed; disabling passthrough");
                } else {
                    volume_keys_debug("startup: listener started");
                }
                rx
            }
        } else {
            volume_keys_debug("startup: passthrough disabled in config");
            None
        };

        Ok(RuntimeState {
            capabilities,
            config,
            tray,
            dev,
            mgr: MediaControl::new(),
            last_time: Local::now() - TimeDelta::seconds(1),
            last_media: None,
            time_layers: vec![],
            media_layers: vec![],
            notif_layers: vec![],
            notif_expiry: Local::now(),
            is_connected: None,
            volume: None,
            needs_redraw: false,
            icon_hs_connect,
            icon_hs_disconnect,
            icon_volume_levels,
            #[cfg(target_os = "macos")]
            volume_key_rx,
        })
    }

    fn save_config(&self) -> anyhow::Result<()> {
        self.config.save()
    }

    fn tick_duration(&self) -> Duration {
        if self.config.pass_through_volume_keys {
            TICK_DUR_FAST
        } else {
            TICK_DUR_NORMAL
        }
    }

    fn clear_notification(&mut self) {
        if !self.notif_layers.is_empty() {
            self.dev.remove_layers(&self.notif_layers);
            self.notif_layers.clear();
        }
    }

    fn show_volume_notification(&mut self, volume: u8) {
        if !self.config.show_notifications {
            return;
        }
        let percent = volume_to_percent(volume);
        let text = format!("{percent}");
        let width = self
            .dev
            .measure_line_widths(&text)
            .first()
            .map(|w| *w as isize)
            .unwrap_or(0);
        let icon = self.icon_volume_levels[volume_icon_level(percent)].clone();
        let total_width = icon.w as isize + VOLUME_ICON_GAP + width;
        let x = (OLED_WIDTH - total_width - NOTIF_MARGIN_X).max(0);
        self.clear_notification();
        self.notif_layers.push(self.dev.add_layer(DrawLayer::ImageNoShift {
            bitmap: icon.clone(),
            x,
            y: NOTIF_MARGIN_Y,
        }));
        self.notif_layers.extend(self.dev.add_text_no_shift(
            &text,
            Some(x + icon.w as isize + VOLUME_ICON_GAP),
            Some(NOTIF_MARGIN_Y),
        ));
        self.notif_expiry = Local::now() + TimeDelta::from_std(NOTIF_DUR).unwrap();
        self.needs_redraw = true;
    }

    fn set_base_station_volume(&mut self, next: u8) {
        let next = next.min(BASE_STATION_VOLUME_MAX);
        let changed = self.volume != Some(next);
        self.dev.set_volume(next);
        self.volume = Some(next);
        if changed {
            self.show_volume_notification(next);
            self.needs_redraw = true;
        }
    }

    #[cfg(target_os = "macos")]
    fn ensure_volume_key_listener(&mut self) {
        if self.volume_key_rx.is_none() && ensure_accessibility_permission(false) {
            volume_keys_debug("ensure listener: trying to start listener");
            self.volume_key_rx = start_volume_key_listener();
            volume_keys_debug(format!(
                "ensure listener: listener present={}",
                self.volume_key_rx.is_some()
            ));
        }
    }

    #[cfg(target_os = "macos")]
    fn handle_volume_key_signal(&mut self, signal: VolumeKeySignal) {
        let current = self.volume.unwrap_or(BASE_STATION_VOLUME_MAX / 2);
        let next = match signal {
            VolumeKeySignal::Up => current
                .saturating_add(BASE_STATION_VOLUME_STEP)
                .min(BASE_STATION_VOLUME_MAX),
            VolumeKeySignal::Down => current.saturating_sub(BASE_STATION_VOLUME_STEP),
            VolumeKeySignal::Mute => 0,
        };
        volume_keys_debug(format!(
            "handle signal: {:?}, current={} -> next={}",
            signal, current, next
        ));
        self.set_base_station_volume(next);
    }

    fn handle_menu_event(&mut self, event: MenuEvent) -> bool {
        if event.id == self.tray.tm_quit.id() {
            return true;
        }

        let mut config_updated = false;

        if event.id == self.tray.tm_time_check.id()
            || event.id == self.tray.tm_media_check.id()
            || event.id == self.tray.tm_media_paused_check.id()
            || event.id == self.tray.tm_notif_check.id()
            || event.id == self.tray.tm_idle_check.id()
        {
            self.config.show_time = self.tray.tm_time_check.is_checked();
            self.config.show_media = self.capabilities.media && self.tray.tm_media_check.is_checked();
            self.config.show_media_paused = self.capabilities.media && self.tray.tm_media_paused_check.is_checked();
            self.config.show_notifications = self.tray.tm_notif_check.is_checked();
            self.config.idle_timeout = self.capabilities.idle_timeout && self.tray.tm_idle_check.is_checked();
            config_updated = true;
        }

        if event.id == self.tray.tm_shift_off.id() {
            self.config.oled_shift = ConfigShiftMode::Off;
            self.tray.tm_shift_off.set_checked(true);
            self.tray.tm_shift_simple.set_checked(false);
            self.dev.set_shift_mode(self.config.oled_shift.to_api());
            config_updated = true;
        }

        if event.id == self.tray.tm_shift_simple.id() {
            self.config.oled_shift = ConfigShiftMode::Simple;
            self.tray.tm_shift_off.set_checked(false);
            self.tray.tm_shift_simple.set_checked(true);
            self.dev.set_shift_mode(self.config.oled_shift.to_api());
            config_updated = true;
        }

        if event.id == self.tray.tm_autostart_check.id() && self.capabilities.autostart {
            self.config.autostart = self.tray.tm_autostart_check.is_checked();
            set_autostart(self.config.autostart);
            config_updated = true;
        }

        #[cfg(target_os = "macos")]
        if event.id == self.tray.tm_pass_through_volume_keys_check.id() {
            self.config.pass_through_volume_keys = self.tray.tm_pass_through_volume_keys_check.is_checked();
            volume_keys_debug(format!(
                "menu: passthrough toggled -> {}",
                self.config.pass_through_volume_keys
            ));
            if self.config.pass_through_volume_keys {
                if !ensure_accessibility_permission(true) {
                    self.config.pass_through_volume_keys = false;
                    self.tray.tm_pass_through_volume_keys_check.set_checked(false);
                    show_error_dialog("macOS Accessibility permission is required for media volume key passthrough.");
                    volume_keys_debug("menu: permission denied after prompt");
                    return false;
                }
                self.ensure_volume_key_listener();
                if self.volume_key_rx.is_none() {
                    self.config.pass_through_volume_keys = false;
                    self.tray.tm_pass_through_volume_keys_check.set_checked(false);
                    show_error_dialog(
                        "Failed to start media volume key passthrough. Ensure Accessibility permissions are granted.",
                    );
                    volume_keys_debug("menu: listener failed to start");
                } else {
                    volume_keys_debug("menu: listener started");
                }
            }
            config_updated = true;
        }

        if event.id == self.tray.tm_volume_down.id()
            || event.id == self.tray.tm_volume_up.id()
            || event.id == self.tray.tm_volume_mute.id()
            || event.id == self.tray.tm_volume_25.id()
            || event.id == self.tray.tm_volume_50.id()
            || event.id == self.tray.tm_volume_75.id()
            || event.id == self.tray.tm_volume_100.id()
        {
            let current = self.volume.unwrap_or(BASE_STATION_VOLUME_MAX / 2);
            let next = if event.id == self.tray.tm_volume_down.id() {
                current.saturating_sub(BASE_STATION_VOLUME_STEP)
            } else if event.id == self.tray.tm_volume_up.id() {
                current
                    .saturating_add(BASE_STATION_VOLUME_STEP)
                    .min(BASE_STATION_VOLUME_MAX)
            } else if event.id == self.tray.tm_volume_mute.id() {
                0
            } else if event.id == self.tray.tm_volume_25.id() {
                volume_from_percent(25)
            } else if event.id == self.tray.tm_volume_50.id() {
                volume_from_percent(50)
            } else if event.id == self.tray.tm_volume_75.id() {
                volume_from_percent(75)
            } else {
                BASE_STATION_VOLUME_MAX
            };
            self.set_base_station_volume(next);
        }

        if config_updated {
            self.needs_redraw = true;
            if let Err(err) = self.save_config() {
                show_error_dialog(&format!("Error saving config: {err:?}"));
            }
        }

        false
    }

    fn tick(&mut self) {
        let mut force_redraw = std::mem::take(&mut self.needs_redraw);

        #[cfg(target_os = "macos")]
        {
            let mut signals = vec![];
            if let Some(rx) = &self.volume_key_rx {
                while let Ok(signal) = rx.try_recv() {
                    signals.push(signal);
                }
            }
            if !signals.is_empty() {
                volume_keys_debug(format!(
                    "tick: drained {} signal(s), passthrough_enabled={}",
                    signals.len(),
                    self.config.pass_through_volume_keys
                ));
            }
            if self.config.pass_through_volume_keys {
                for signal in signals {
                    self.handle_volume_key_signal(signal);
                    force_redraw = true;
                }
            } else if !signals.is_empty() {
                volume_keys_debug("tick: dropped signals because passthrough is disabled");
            }
        }

        while let Some(event) = self.dev.try_event() {
            debug!(?event, "draw event");
            match event {
                DrawEvent::DeviceDisconnected => _ = self.tray.tray.set_icon(Some(self.tray.icon_error.clone())),
                DrawEvent::DeviceReconnected => _ = self.tray.tray.set_icon(Some(self.tray.icon_ok.clone())),
                #[allow(clippy::single_match)]
                DrawEvent::DeviceEvent(event) => match event {
                    ggoled_lib::DeviceEvent::Volume { volume } => {
                        let volume = volume.min(BASE_STATION_VOLUME_MAX);
                        let changed = self.volume != Some(volume);
                        self.volume = Some(volume);
                        if changed {
                            self.show_volume_notification(volume);
                            force_redraw = true;
                        }
                    }
                    ggoled_lib::DeviceEvent::HeadsetConnection { wireless, .. } => {
                        if Some(wireless) != self.is_connected {
                            self.is_connected = Some(wireless);
                            if self.config.show_notifications {
                                self.clear_notification();
                                self.notif_layers.push(
                                    self.dev.add_layer(ggoled_draw::DrawLayer::ImageNoShift {
                                        bitmap: (if wireless {
                                            &self.icon_hs_connect
                                        } else {
                                            &self.icon_hs_disconnect
                                        })
                                        .clone(),
                                        x: 8,
                                        y: 8,
                                    }),
                                );
                                self.notif_expiry = Local::now() + TimeDelta::from_std(NOTIF_DUR).unwrap();
                                force_redraw = true;
                            }
                        }
                    }
                    _ => {}
                },
            }
        }

        let time = Local::now();
        if time.second() == self.last_time.second() && !force_redraw {
            return;
        }
        self.last_time = time;

        if !self.notif_layers.is_empty() && time >= self.notif_expiry {
            self.clear_notification();
        }

        let idle_seconds = get_idle_seconds();
        if self.config.idle_timeout && idle_seconds >= IDLE_TIMEOUT_SECS {
            self.dev.clear_layers();
            self.last_media = None;
            return;
        }

        let media = if self.config.show_media {
            self.mgr.get_media(self.config.show_media_paused)
        } else {
            None
        };

        let time_y = if media.is_some() { Some(8) } else { None };
        let time_str = self.config.show_time.then(|| time.format("%I:%M:%S %p").to_string());
        let media_changed = media != self.last_media;
        let media_text = media
            .as_ref()
            .map(|m| format!("{}\n{}", m.title, m.artist))
            .filter(|_| media_changed);
        let old_time_layers = std::mem::take(&mut self.time_layers);
        let old_media_layers = if media_changed {
            std::mem::take(&mut self.media_layers)
        } else {
            vec![]
        };
        let mut new_time_layers = vec![];
        let mut new_media_layers = vec![];
        let media_y = 8 + self.dev.font_line_height() as isize;
        self.dev.transact_layers(|txn| {
            txn.remove_layers(&old_time_layers);
            if let Some(time_str) = &time_str {
                new_time_layers = txn.add_text_with_mode(time_str, None, time_y, true, TextOverflowMode::Scroll);
            }
            if media_changed {
                txn.remove_layers(&old_media_layers);
                if let Some(media_text) = &media_text {
                    new_media_layers =
                        txn.add_text_with_mode(media_text, None, Some(media_y), true, TextOverflowMode::Scroll);
                }
            }
        });
        self.time_layers = new_time_layers;
        if media_changed {
            self.media_layers = new_media_layers;
            self.last_media = media;
        }
    }

    fn shutdown(self) {
        let dev = self.dev.stop();
        _ = dev.return_to_ui();
    }
}

fn load_tray_icon(buf: &[u8]) -> anyhow::Result<TrayIconImage> {
    let pixels = image::load_from_memory(buf)?
        .resize(32, 32, image::imageops::FilterType::Lanczos3)
        .to_rgba8()
        .into_vec();
    let icon = TrayIconImage::from_rgba(pixels, 32, 32)?;
    Ok(icon)
}

fn create_tray(config: &Config, capabilities: PlatformCapabilities) -> anyhow::Result<TrayState> {
    let icon_ok = load_tray_icon(include_bytes!("../assets/ggoled.png"))?;
    let icon_error = load_tray_icon(include_bytes!("../assets/ggoled_error.png"))?;

    let menu = Menu::new();

    let tm_time_check = CheckMenuItem::new("Show time", true, config.show_time, None);
    let tm_media_check = CheckMenuItem::new("Show playing media", true, config.show_media, None);
    let tm_media_paused_check = CheckMenuItem::new("Show paused media", true, config.show_media_paused, None);
    let tm_notif_check = CheckMenuItem::new("Show connection notifications", true, config.show_notifications, None);
    let tm_idle_check = CheckMenuItem::new("Screensaver when idle", true, config.idle_timeout, None);
    let tm_autostart_check = CheckMenuItem::new("Start at login", true, config.autostart, None);
    let tm_volume_down = MenuItem::new("Volume down", true, None);
    let tm_volume_up = MenuItem::new("Volume up", true, None);
    let tm_volume_mute = MenuItem::new("Mute", true, None);
    let tm_volume_25 = MenuItem::new("25%", true, None);
    let tm_volume_50 = MenuItem::new("50%", true, None);
    let tm_volume_75 = MenuItem::new("75%", true, None);
    let tm_volume_100 = MenuItem::new("100%", true, None);
    #[cfg(target_os = "macos")]
    let tm_pass_through_volume_keys_check = CheckMenuItem::new(
        "Pass through media volume keys",
        true,
        config.pass_through_volume_keys,
        None,
    );

    menu.append(&tm_time_check)?;
    menu.append(&tm_media_check)?;
    menu.append(&tm_media_paused_check)?;
    menu.append(&tm_notif_check)?;
    menu.append(&tm_idle_check)?;
    menu.append(&tm_autostart_check)?;

    let tm_volume_submenu = Submenu::new("Base station volume", true);
    tm_volume_submenu.append(&tm_volume_down)?;
    tm_volume_submenu.append(&tm_volume_up)?;
    tm_volume_submenu.append(&tm_volume_mute)?;
    tm_volume_submenu.append(&tm_volume_25)?;
    tm_volume_submenu.append(&tm_volume_50)?;
    tm_volume_submenu.append(&tm_volume_75)?;
    tm_volume_submenu.append(&tm_volume_100)?;
    menu.append(&tm_volume_submenu)?;
    #[cfg(target_os = "macos")]
    menu.append(&tm_pass_through_volume_keys_check)?;

    let tm_shift_submenu = Submenu::new("OLED screen shift", true);
    let tm_shift_off = CheckMenuItem::new("Off", true, matches!(config.oled_shift, ConfigShiftMode::Off), None);
    let tm_shift_simple = CheckMenuItem::new(
        "Simple",
        true,
        matches!(config.oled_shift, ConfigShiftMode::Simple),
        None,
    );
    tm_shift_submenu.append(&tm_shift_off)?;
    tm_shift_submenu.append(&tm_shift_simple)?;
    menu.append(&tm_shift_submenu)?;

    let tm_quit = MenuItem::new("Quit", true, None);
    menu.append(&tm_quit)?;

    if !capabilities.media {
        tm_media_check.set_checked(false);
        tm_media_check.set_enabled(false);
        tm_media_paused_check.set_checked(false);
        tm_media_paused_check.set_enabled(false);
    }
    if !capabilities.idle_timeout {
        tm_idle_check.set_checked(false);
        tm_idle_check.set_enabled(false);
    }
    if !capabilities.autostart {
        tm_autostart_check.set_checked(false);
        tm_autostart_check.set_enabled(false);
    }

    let tray = TrayIconBuilder::new()
        .with_tooltip("ggoled")
        .with_icon(icon_ok.clone())
        .with_menu(Box::new(menu))
        .build()?;

    Ok(TrayState {
        tray,
        icon_ok,
        icon_error,
        tm_time_check,
        tm_media_check,
        tm_media_paused_check,
        tm_notif_check,
        tm_idle_check,
        tm_autostart_check,
        tm_volume_down,
        tm_volume_up,
        tm_volume_mute,
        tm_volume_25,
        tm_volume_50,
        tm_volume_75,
        tm_volume_100,
        #[cfg(target_os = "macos")]
        tm_pass_through_volume_keys_check,
        tm_shift_off,
        tm_shift_simple,
        tm_quit,
    })
}

fn main() {
    init_tracing();
    info!("ggoled_app starting");
    let mut config = Config::load();
    let capabilities = capabilities();

    config.show_media = config.show_media && capabilities.media;
    config.show_media_paused = config.show_media_paused && capabilities.media;
    config.idle_timeout = config.idle_timeout && capabilities.idle_timeout;
    config.autostart = if capabilities.autostart { get_autostart() } else { false };
    #[cfg(not(target_os = "macos"))]
    {
        config.pass_through_volume_keys = false;
    }

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        // Tray app behavior: no Dock tile and no app switcher presence.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_dock_visibility(false);
    }

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        _ = proxy.send_event(UserEvent::MenuEvent(event));
    }));
    let proxy = event_loop.create_proxy();
    if let Err(err) = ctrlc::set_handler(move || {
        _ = proxy.send_event(UserEvent::ShutdownRequested);
    }) {
        warn!(?err, "failed to register shutdown signal handler");
    }

    let mut runtime: Option<RuntimeState> = None;
    let mut initial_config = Some(config);

    event_loop.run(move |event, _, control_flow| {
        let tick_dur = runtime.as_ref().map_or(TICK_DUR_NORMAL, RuntimeState::tick_duration);
        *control_flow = ControlFlow::WaitUntil(std::time::Instant::now() + tick_dur);

        match event {
            Event::NewEvents(StartCause::Init) => {
                let Some(config) = initial_config.take() else {
                    *control_flow = ControlFlow::Exit;
                    return;
                };
                let tray = match create_tray(&config, capabilities) {
                    Ok(tray) => tray,
                    Err(err) => {
                        show_error_dialog(&format!("Error creating tray: {err:?}"));
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                };
                match RuntimeState::new(config, capabilities, tray) {
                    Ok(state) => runtime = Some(state),
                    Err(err) => {
                        show_error_dialog(&format!("Error: {err:?}"));
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::MenuEvent(event)) => {
                if let Some(state) = runtime.as_mut() {
                    let should_quit = state.handle_menu_event(event);
                    if should_quit {
                        if let Some(state) = runtime.take() {
                            state.shutdown();
                        }
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::ShutdownRequested) => {
                if let Some(state) = runtime.take() {
                    state.shutdown();
                }
                *control_flow = ControlFlow::Exit;
            }
            Event::MainEventsCleared => {
                if let Some(state) = runtime.as_mut() {
                    state.tick();
                }
            }
            Event::LoopDestroyed => {
                if let Some(state) = runtime.take() {
                    state.shutdown();
                }
            }
            _ => {}
        }
    });
}
