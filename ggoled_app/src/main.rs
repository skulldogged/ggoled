#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod os;

use chrono::{DateTime, Local, TimeDelta, Timelike};
use ggoled_draw::{bitmap_from_memory, DrawDevice, DrawEvent, LayerId, ShiftMode, TextRenderer};
use ggoled_lib::Device;
use os::{capabilities, get_autostart, get_idle_seconds, set_autostart, Media, MediaControl, PlatformCapabilities};
use rfd::{MessageDialog, MessageLevel};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, Submenu},
    Icon as TrayIconImage, TrayIcon, TrayIconBuilder,
};

const IDLE_TIMEOUT_SECS: usize = 60;
const NOTIF_DUR: Duration = Duration::from_secs(5);
const TICK_DUR: Duration = Duration::from_millis(10);

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
    notif_layer: Option<LayerId>,
    notif_expiry: DateTime<Local>,
    is_connected: Option<bool>,
    needs_redraw: bool,
    icon_hs_connect: Arc<ggoled_lib::Bitmap>,
    icon_hs_disconnect: Arc<ggoled_lib::Bitmap>,
}

enum UserEvent {
    MenuEvent(MenuEvent),
}

impl RuntimeState {
    fn new(config: Config, capabilities: PlatformCapabilities, tray: TrayState) -> anyhow::Result<RuntimeState> {
        let icon_hs_connect =
            Arc::new(bitmap_from_memory(include_bytes!("../assets/headset_connected.png"), 0x80).unwrap());
        let icon_hs_disconnect =
            Arc::new(bitmap_from_memory(include_bytes!("../assets/headset_disconnected.png"), 0x80).unwrap());

        let mut dev = DrawDevice::new(Device::connect()?, 30);
        if let Some(font) = &config.font {
            dev.texter = TextRenderer::load_from_file(&font.path, font.size)?;
        }

        dev.set_shift_mode(config.oled_shift.to_api());
        dev.play();

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
            notif_layer: None,
            notif_expiry: Local::now(),
            is_connected: None,
            needs_redraw: false,
            icon_hs_connect,
            icon_hs_disconnect,
        })
    }

    fn save_config(&self) -> anyhow::Result<()> {
        self.config.save()
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

        while let Some(event) = self.dev.try_event() {
            println!("event: {:?}", event);
            match event {
                DrawEvent::DeviceDisconnected => _ = self.tray.tray.set_icon(Some(self.tray.icon_error.clone())),
                DrawEvent::DeviceReconnected => _ = self.tray.tray.set_icon(Some(self.tray.icon_ok.clone())),
                #[allow(clippy::single_match)]
                DrawEvent::DeviceEvent(event) => match event {
                    ggoled_lib::DeviceEvent::HeadsetConnection { wireless, .. } => {
                        if Some(wireless) != self.is_connected {
                            self.is_connected = Some(wireless);
                            if self.config.show_notifications {
                                if let Some(id) = self.notif_layer {
                                    self.dev.remove_layer(id);
                                }
                                self.notif_layer = Some(
                                    self.dev.add_layer(ggoled_draw::DrawLayer::Image {
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

        if let Some(id) = self.notif_layer {
            if time >= self.notif_expiry {
                self.dev.remove_layer(id);
                self.notif_layer = None;
            }
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

        self.dev.pause();

        self.dev.remove_layers(&self.time_layers);
        if self.config.show_time {
            let time_str = time.format("%I:%M:%S %p").to_string();
            self.time_layers = self
                .dev
                .add_text(&time_str, None, if media.is_some() { Some(8) } else { None });
        }

        if media != self.last_media {
            self.dev.remove_layers(&self.media_layers);
            if let Some(m) = &media {
                self.media_layers = self.dev.add_text(
                    &format!("{}\n{}", m.title, m.artist),
                    None,
                    Some(8 + self.dev.font_line_height() as isize),
                );
            }
            self.last_media = media;
        }

        self.dev.play();
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

    menu.append(&tm_time_check)?;
    menu.append(&tm_media_check)?;
    menu.append(&tm_media_paused_check)?;
    menu.append(&tm_notif_check)?;
    menu.append(&tm_idle_check)?;
    menu.append(&tm_autostart_check)?;

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
        tm_shift_off,
        tm_shift_simple,
        tm_quit,
    })
}

fn main() {
    let mut config = Config::load();
    let capabilities = capabilities();

    config.show_media = config.show_media && capabilities.media;
    config.show_media_paused = config.show_media_paused && capabilities.media;
    config.idle_timeout = config.idle_timeout && capabilities.idle_timeout;
    config.autostart = if capabilities.autostart { get_autostart() } else { false };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        _ = proxy.send_event(UserEvent::MenuEvent(event));
    }));

    let mut runtime: Option<RuntimeState> = None;
    let mut initial_config = Some(config);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(std::time::Instant::now() + TICK_DUR);

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
