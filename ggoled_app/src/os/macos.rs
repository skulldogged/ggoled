use super::macos_mediaremote::NowPlaying;
use super::{Media, PlatformCapabilities, VolumeKeySignal};
use auto_launch::{AutoLaunch, MacOSLaunchMode};
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::number::CFNumber;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, EventField,
};
use core_graphics::sys::CGEventRef;
use dispatch2::ffi::{dispatch_queue_create, dispatch_queue_t, DISPATCH_QUEUE_SERIAL};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, warn};

const APP_NAME: &str = "ggoled_app";
const APP_BUNDLE_IDENTIFIER: &str = "com.apple.ggoled";

static MEDIA_INIT_LOGGED: AtomicBool = AtomicBool::new(false);
static MEDIA_READ_LOGGED: AtomicBool = AtomicBool::new(false);
static IDLE_LOGGED: AtomicBool = AtomicBool::new(false);
static AUTOSTART_INIT_LOGGED: AtomicBool = AtomicBool::new(false);
static AUTOSTART_SET_LOGGED: AtomicBool = AtomicBool::new(false);
static AUTOSTART_GET_LOGGED: AtomicBool = AtomicBool::new(false);
type IOHIDManagerRef = *mut c_void;
type IOHIDValueRef = *mut c_void;
type IOHIDElementRef = *mut c_void;
type IOReturn = i32;
type RawCGEventTapCallBack =
    unsafe extern "C" fn(proxy: *const c_void, etype: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef;
type IOHIDValueCallback =
    unsafe extern "C" fn(context: *mut c_void, result: IOReturn, sender: *mut c_void, value: IOHIDValueRef);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(theDict: CFDictionaryRef) -> bool;
    fn AXIsProcessTrusted() -> bool;
    fn CGEventTapCreate(
        tap: CGEventTapLocation,
        place: CGEventTapPlacement,
        options: CGEventTapOptions,
        eventsOfInterest: u64,
        callback: RawCGEventTapCallBack,
        userInfo: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDRequestAccess(request_type: i32) -> bool;
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDManagerSetDeviceMatchingMultiple(manager: IOHIDManagerRef, multiple: *const c_void);
    fn IOHIDManagerSetDispatchQueue(manager: IOHIDManagerRef, queue: dispatch_queue_t);
    fn IOHIDManagerActivate(manager: IOHIDManagerRef);
    fn IOHIDManagerRegisterInputValueCallback(
        manager: IOHIDManagerRef,
        callback: Option<IOHIDValueCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerSetInputValueMatchingMultiple(manager: IOHIDManagerRef, multiple: *const c_void);
    fn IOHIDValueGetElement(value: IOHIDValueRef) -> IOHIDElementRef;
    fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> isize;
    fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
}

fn log_once(flag: &AtomicBool, msg: impl AsRef<str>) {
    if flag
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        warn!("{}", msg.as_ref());
    }
}

fn media_debug(msg: impl AsRef<str>) {
    debug!(target: "media", "{}", msg.as_ref());
}

fn volume_key_debug(msg: impl AsRef<str>) {
    debug!(target: "volume-keys", "{}", msg.as_ref());
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

pub fn ensure_accessibility_permission(prompt: bool) -> bool {
    let trusted = unsafe {
        if prompt {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let value = CFBoolean::true_value();
            let options: CFDictionary<CFString, CFBoolean> = CFDictionary::from_CFType_pairs(&[(key, value)]);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef())
        } else {
            AXIsProcessTrusted()
        }
    };
    volume_key_debug(format!(
        "accessibility permission check (prompt={prompt}) => trusted={trusted}"
    ));
    trusted
}

pub fn start_volume_key_listener() -> Option<Receiver<VolumeKeySignal>> {
    const MEDIA_KEYCODE_VOLUME_UP: i64 = 72;
    const MEDIA_KEYCODE_VOLUME_DOWN: i64 = 73;
    const MEDIA_KEYCODE_VOLUME_MUTE: i64 = 74;
    const FKEYCODE_F10: i64 = 109;
    const FKEYCODE_F11: i64 = 103;
    const FKEYCODE_F12: i64 = 111;
    const K_IO_HID_REQUEST_TYPE_LISTEN_EVENT: i32 = 1;
    const K_IO_HID_PAGE_CONSUMER: u32 = 0x0C;
    const K_IO_HID_USAGE_CONSUMER_CONTROL: u32 = 0x01;
    const K_IO_HID_USAGE_CONSUMER_MUTE: u32 = 0xE2;
    const K_IO_HID_USAGE_CONSUMER_VOLUME_INCREMENT: u32 = 0xE9;
    const K_IO_HID_USAGE_CONSUMER_VOLUME_DECREMENT: u32 = 0xEA;
    const SYSTEM_DEFINED_EVENT_TYPE: u32 = 14;
    const SUPPRESS_WINDOW_MS: u64 = 150;
    const HID_DUPLICATE_WINDOW_MS: u64 = 12;

    fn now_unix_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    struct SuppressState {
        suppress_until_ms: AtomicU64,
    }

    struct HidDedupeState {
        usage: u32,
        value: isize,
        at_ms: u64,
    }

    struct HidListenerContext {
        tx: Sender<VolumeKeySignal>,
        suppress_state: Arc<SuppressState>,
        dedupe: Arc<Mutex<HidDedupeState>>,
    }

    unsafe extern "C" fn hid_input_value_callback(
        context: *mut c_void,
        result: IOReturn,
        _sender: *mut c_void,
        value: IOHIDValueRef,
    ) {
        if result != 0 || context.is_null() || value.is_null() {
            return;
        }
        let ctx = unsafe { &*(context as *const HidListenerContext) };
        let element = unsafe { IOHIDValueGetElement(value) };
        if element.is_null() {
            return;
        }
        let usage_page = unsafe { IOHIDElementGetUsagePage(element) };
        let usage = unsafe { IOHIDElementGetUsage(element) };
        let raw_value = unsafe { IOHIDValueGetIntegerValue(value) };
        let signal = if usage_page == K_IO_HID_PAGE_CONSUMER && raw_value != 0 {
            match usage {
                K_IO_HID_USAGE_CONSUMER_VOLUME_INCREMENT => Some(VolumeKeySignal::Up),
                K_IO_HID_USAGE_CONSUMER_VOLUME_DECREMENT => Some(VolumeKeySignal::Down),
                K_IO_HID_USAGE_CONSUMER_MUTE => Some(VolumeKeySignal::Mute),
                _ => None,
            }
        } else {
            None
        };
        volume_key_debug(format!(
            "hid input: page=0x{usage_page:02x} usage=0x{usage:03x} value={} mapped={:?}",
            raw_value, signal
        ));
        if let Some(signal) = signal {
            let now_ms = now_unix_millis();
            let duplicate = {
                let mut dedupe = ctx.dedupe.lock().expect("hid dedupe mutex poisoned");
                let is_dup = dedupe.usage == usage
                    && dedupe.value == raw_value
                    && now_ms.saturating_sub(dedupe.at_ms) <= HID_DUPLICATE_WINDOW_MS;
                if !is_dup {
                    dedupe.usage = usage;
                    dedupe.value = raw_value;
                    dedupe.at_ms = now_ms;
                }
                is_dup
            };
            if duplicate {
                volume_key_debug(format!(
                    "hid duplicate suppressed: usage=0x{usage:03x} value={} now_ms={}",
                    raw_value, now_ms
                ));
                return;
            }
            let suppress_until_ms = now_unix_millis().saturating_add(SUPPRESS_WINDOW_MS);
            ctx.suppress_state
                .suppress_until_ms
                .store(suppress_until_ms, Ordering::Relaxed);
            volume_key_debug(format!(
                "hid mapped {:?}; suppress_until_ms={}",
                signal, suppress_until_ms
            ));
            if ctx.tx.send(signal).is_err() {
                volume_key_debug("receiver dropped; ignoring HID consumer key");
            }
        }
    }

    unsafe extern "C" fn system_defined_suppress_callback(
        _proxy: *const c_void,
        etype: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef {
        if user_info.is_null() {
            return event;
        }
        if etype == CGEventType::TapDisabledByTimeout as u32 || etype == CGEventType::TapDisabledByUserInput as u32 {
            volume_key_debug(format!("system-defined suppressor tap disabled: type=0x{etype:08x}"));
            return event;
        }
        let suppress_state = unsafe { &*(user_info as *const Arc<SuppressState>) };
        let now_ms = now_unix_millis();
        let suppress_until_ms = suppress_state.suppress_until_ms.load(Ordering::Relaxed);
        if now_ms <= suppress_until_ms {
            if etype == SYSTEM_DEFINED_EVENT_TYPE {
                volume_key_debug(format!(
                    "suppressed system-defined event in window now_ms={} suppress_until_ms={}",
                    now_ms, suppress_until_ms
                ));
                return std::ptr::null_mut();
            }
            if etype == CGEventType::KeyDown as u32 {
                let keycode = unsafe { CGEventGetIntegerValueField(event, EventField::KEYBOARD_EVENT_KEYCODE as i32) };
                if matches!(
                    keycode,
                    MEDIA_KEYCODE_VOLUME_UP | MEDIA_KEYCODE_VOLUME_DOWN | MEDIA_KEYCODE_VOLUME_MUTE
                ) {
                    volume_key_debug(format!(
                        "suppressed media keydown keycode={} in window now_ms={} suppress_until_ms={}",
                        keycode, now_ms, suppress_until_ms
                    ));
                    return std::ptr::null_mut();
                }
            }
        }
        event
    }

    fn start_system_defined_suppressor_listener(suppress_state: Arc<SuppressState>) -> bool {
        let res = std::thread::Builder::new()
            .name("ggoled-volume-keys-suppressor".to_string())
            .spawn(move || {
                volume_key_debug("system-defined suppressor listener thread started");
                let state_ptr = Box::into_raw(Box::new(suppress_state)) as *mut c_void;
                let event_mask = (1_u64 << SYSTEM_DEFINED_EVENT_TYPE) | (1_u64 << (CGEventType::KeyDown as u32));
                let tap = unsafe {
                    CGEventTapCreate(
                        CGEventTapLocation::Session,
                        CGEventTapPlacement::HeadInsertEventTap,
                        CGEventTapOptions::Default,
                        event_mask,
                        system_defined_suppress_callback,
                        state_ptr,
                    )
                };
                if tap.is_null() {
                    error!("failed to create system-defined suppressor event tap");
                    volume_key_debug("failed to create system-defined suppressor event tap");
                    unsafe {
                        drop(Box::from_raw(state_ptr as *mut Arc<SuppressState>));
                    }
                    return;
                }
                volume_key_debug("created system-defined suppressor event tap");
                let tap = unsafe { CFMachPort::wrap_under_create_rule(tap) };
                let Ok(loop_source) = tap.create_runloop_source(0) else {
                    error!("failed to create runloop source for system-defined suppressor listener");
                    volume_key_debug("failed to create runloop source for system-defined suppressor listener");
                    unsafe {
                        drop(Box::from_raw(state_ptr as *mut Arc<SuppressState>));
                    }
                    return;
                };
                let run_loop = CFRunLoop::get_current();
                unsafe {
                    run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
                    CGEventTapEnable(tap.as_concrete_TypeRef(), true);
                }
                volume_key_debug("system-defined suppressor listener runloop active");
                CFRunLoop::run_current();
                volume_key_debug("system-defined suppressor listener runloop exited");
                unsafe {
                    drop(Box::from_raw(state_ptr as *mut Arc<SuppressState>));
                }
            });
        if let Err(err) = res {
            error!(?err, "failed to start system-defined suppressor listener thread");
            volume_key_debug(format!("failed to spawn system-defined suppressor listener: {err}"));
            return false;
        }
        volume_key_debug("system-defined suppressor listener thread spawned successfully");
        true
    }

    fn start_hid_listener(tx: Sender<VolumeKeySignal>, suppress_state: Arc<SuppressState>) -> bool {
        let res = std::thread::Builder::new()
            .name("ggoled-volume-keys-hid".to_string())
            .spawn(move || {
                volume_key_debug("hid volume key listener thread started");
                let tx_ptr = Box::into_raw(Box::new(HidListenerContext {
                    tx,
                    suppress_state,
                    dedupe: Arc::new(Mutex::new(HidDedupeState {
                        usage: 0,
                        value: 0,
                        at_ms: 0,
                    })),
                })) as *mut c_void;
                let manager = unsafe { IOHIDManagerCreate(std::ptr::null(), 0) };
                if manager.is_null() {
                    error!("failed to create IOHIDManager");
                    volume_key_debug("failed to create IOHIDManager");
                    unsafe { drop(Box::from_raw(tx_ptr as *mut HidListenerContext)) };
                    return;
                }

                let usage_page_key = CFString::from_static_string("UsagePage");
                let usage_key = CFString::from_static_string("Usage");
                let usage_page = CFNumber::from(K_IO_HID_PAGE_CONSUMER as i64);
                let match_dicts = [
                    CFDictionary::from_CFType_pairs(&[
                        (usage_page_key.clone(), usage_page.clone()),
                        (
                            usage_key.clone(),
                            CFNumber::from(K_IO_HID_USAGE_CONSUMER_VOLUME_INCREMENT as i64),
                        ),
                    ]),
                    CFDictionary::from_CFType_pairs(&[
                        (usage_page_key.clone(), usage_page.clone()),
                        (
                            usage_key.clone(),
                            CFNumber::from(K_IO_HID_USAGE_CONSUMER_VOLUME_DECREMENT as i64),
                        ),
                    ]),
                    CFDictionary::from_CFType_pairs(&[
                        (usage_page_key, usage_page),
                        (usage_key, CFNumber::from(K_IO_HID_USAGE_CONSUMER_MUTE as i64)),
                    ]),
                ];
                let matches: CFArray<CFDictionary<CFString, CFNumber>> = CFArray::from_CFTypes(&match_dicts);
                let device_usage_page_key = CFString::from_static_string("DeviceUsagePage");
                let device_usage_key = CFString::from_static_string("DeviceUsage");
                let device_matches = [CFDictionary::from_CFType_pairs(&[
                    (device_usage_page_key, CFNumber::from(K_IO_HID_PAGE_CONSUMER as i64)),
                    (device_usage_key, CFNumber::from(K_IO_HID_USAGE_CONSUMER_CONTROL as i64)),
                ])];
                let device_matches: CFArray<CFDictionary<CFString, CFNumber>> = CFArray::from_CFTypes(&device_matches);
                unsafe {
                    IOHIDManagerSetDeviceMatchingMultiple(
                        manager,
                        device_matches.as_concrete_TypeRef() as *const c_void,
                    );
                    IOHIDManagerSetInputValueMatchingMultiple(manager, matches.as_concrete_TypeRef() as *const c_void);
                    IOHIDManagerRegisterInputValueCallback(manager, Some(hid_input_value_callback), tx_ptr);
                }
                let access = unsafe { IOHIDRequestAccess(K_IO_HID_REQUEST_TYPE_LISTEN_EVENT) };
                volume_key_debug(format!("IOHID listen-event access granted={access}"));
                let open_result = unsafe { IOHIDManagerOpen(manager, 0) };
                if open_result != 0 {
                    error!(open_result, "failed to open IOHIDManager for input listening");
                    volume_key_debug(format!("IOHIDManagerOpen failed with code={open_result}"));
                    unsafe { drop(Box::from_raw(tx_ptr as *mut HidListenerContext)) };
                    return;
                }
                let queue = unsafe { dispatch_queue_create(std::ptr::null(), DISPATCH_QUEUE_SERIAL) };
                unsafe {
                    IOHIDManagerSetDispatchQueue(manager, queue);
                    IOHIDManagerActivate(manager);
                }
                volume_key_debug("hid volume key listener dispatch queue active");

                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            });
        if let Err(err) = res {
            error!(?err, "failed to start hid volume key listener thread");
            volume_key_debug(format!("failed to spawn hid listener thread: {err}"));
            return false;
        }
        volume_key_debug("hid volume key listener thread spawned successfully");
        true
    }

    fn start_keydown_tap_listener(tx: Sender<VolumeKeySignal>, suppress_state: Arc<SuppressState>) -> bool {
        let res = std::thread::Builder::new()
            .name("ggoled-volume-keys".to_string())
            .spawn(move || {
                volume_key_debug("volume key listener thread started");
                let tap = CGEventTap::new(
                    CGEventTapLocation::HID,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    vec![CGEventType::KeyDown],
                    move |_proxy, event_type, event| {
                        if matches!(
                            event_type,
                            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                        ) {
                            volume_key_debug(format!("received tap disable event: {:?}", event_type));
                        }
                        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                        let repeat = event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT);
                        let signal = match keycode {
                            key if key == MEDIA_KEYCODE_VOLUME_UP || key == FKEYCODE_F12 => Some(VolumeKeySignal::Up),
                            key if key == MEDIA_KEYCODE_VOLUME_DOWN || key == FKEYCODE_F11 => {
                                Some(VolumeKeySignal::Down)
                            }
                            key if key == MEDIA_KEYCODE_VOLUME_MUTE || key == FKEYCODE_F10 => {
                                Some(VolumeKeySignal::Mute)
                            }
                            _ => None,
                        };
                        volume_key_debug(format!(
                            "event: type={:?} keycode={} autorepeat={} mapped={:?}",
                            event_type, keycode, repeat, signal
                        ));
                        if let Some(signal) = signal {
                            let suppress_until_ms = now_unix_millis().saturating_add(SUPPRESS_WINDOW_MS);
                            suppress_state
                                .suppress_until_ms
                                .store(suppress_until_ms, Ordering::Relaxed);
                            volume_key_debug(format!(
                                "keydown mapped {:?}; suppress_until_ms={}",
                                signal, suppress_until_ms
                            ));
                            if tx.send(signal).is_err() {
                                volume_key_debug("receiver dropped; stopping volume key listener callback");
                            }
                        }
                        None
                    },
                );
                let Ok(tap) = tap else {
                    error!("failed to create volume key event tap");
                    volume_key_debug("failed to create volume key event tap");
                    return;
                };
                volume_key_debug("created volume key event tap");
                let Ok(loop_source) = tap.mach_port.create_runloop_source(0) else {
                    error!("failed to create runloop source for volume key listener");
                    volume_key_debug("failed to create runloop source for volume key listener");
                    return;
                };
                let run_loop = CFRunLoop::get_current();
                unsafe {
                    run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
                }
                tap.enable();
                volume_key_debug("volume key listener runloop active");
                CFRunLoop::run_current();
                volume_key_debug("volume key listener runloop exited");
            });
        if let Err(err) = res {
            error!(?err, "failed to start volume key listener thread");
            volume_key_debug(format!("failed to spawn listener thread: {err}"));
            return false;
        }
        volume_key_debug("volume key listener thread spawned successfully");
        true
    }

    volume_key_debug("starting volume key listener thread");
    let (tx, rx) = channel::<VolumeKeySignal>();
    let suppress_state = Arc::new(SuppressState {
        suppress_until_ms: AtomicU64::new(0),
    });
    let hid_started = start_hid_listener(tx.clone(), suppress_state.clone());
    let keydown_tap_started = start_keydown_tap_listener(tx, suppress_state.clone());
    let suppressor_started = if hid_started || keydown_tap_started {
        start_system_defined_suppressor_listener(suppress_state)
    } else {
        false
    };
    if (hid_started || keydown_tap_started) && !suppressor_started {
        warn!("media key passthrough started without system-defined suppressor; stock volume UI may still appear");
    }
    if !hid_started && !keydown_tap_started {
        return None;
    }
    Some(rx)
}
