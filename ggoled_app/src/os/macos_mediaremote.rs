use block2::RcBlock;
use core::ffi::c_int;
use dispatch2::ffi::{dispatch_queue_create, dispatch_queue_s, DISPATCH_QUEUE_SERIAL};
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSString};
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::OnceLock;
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard};
use std::time::Duration;

const TIMEOUT_DURATION: Duration = Duration::from_secs(2);
const TITLE_KEY: &str = "kMRMediaRemoteNowPlayingInfoTitle";
const ARTIST_KEY: &str = "kMRMediaRemoteNowPlayingInfoArtist";
const NOTIF_INFO_CHANGE: &str = "kMRMediaRemoteNowPlayingInfoDidChangeNotification";
const NOTIF_PLAYING_CHANGE: &str = "kMRMediaRemoteNowPlayingApplicationIsPlayingDidChangeNotification";

type Observer = Retained<AnyObject>;

#[derive(Default, Debug, Clone)]
pub struct NowPlayingInfo {
    pub is_playing: Option<bool>,
    pub title: Option<String>,
    pub artist: Option<String>,
}

pub struct NowPlaying {
    info: Arc<RwLock<NowPlayingInfo>>,
    observers: Vec<Observer>,
}

fn debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("GGOLED_MEDIAREMOTE_DEBUG") {
        Ok(value) => {
            let lower = value.trim().to_ascii_lowercase();
            !(lower.is_empty() || lower == "0" || lower == "false" || lower == "off")
        }
        Err(_) => false,
    })
}

macro_rules! mr_debug {
    ($($arg:tt)*) => {
        if debug_enabled() {
            eprintln!("[mediaremote] {}", format!($($arg)*));
        }
    };
}

macro_rules! safely_dispatch_and_wait {
    ($closure:expr, $type:ty, $func:ident) => {{
        let result = Arc::new((Mutex::new(None), Condvar::new()));

        let result_clone = Arc::clone(&result);
        let block = RcBlock::new(move |arg: $type| {
            let (lock, cvar) = &*result_clone;
            let mut result_guard = lock.lock().unwrap();
            *result_guard = $closure(arg);
            cvar.notify_one();
        });

        unsafe {
            let queue = media_remote_queue();
            if queue.is_null() {
                None
            } else {
                $func(queue, &block);

                let (lock, cvar) = &*result;
                let result_guard = match lock.lock() {
                    Ok(guard) => guard,
                    Err(_) => return None,
                };
                let (result_guard, timeout_result) = match cvar.wait_timeout(result_guard, TIMEOUT_DURATION) {
                    Ok(res) => res,
                    Err(_) => return None,
                };

                if timeout_result.timed_out() {
                    None
                } else {
                    result_guard.clone()
                }
            }
        }
    }};
}

fn media_remote_queue() -> *mut dispatch_queue_s {
    static QUEUE: OnceLock<usize> = OnceLock::new();
    *QUEUE.get_or_init(|| unsafe {
        let queue = dispatch_queue_create(ptr::null(), DISPATCH_QUEUE_SERIAL);
        mr_debug!("created dispatch queue: {:p}", queue);
        queue as usize
    }) as *mut dispatch_queue_s
}

fn query_is_playing() -> Option<bool> {
    let result = safely_dispatch_and_wait!(
        |is_playing: c_int| Some(is_playing != 0),
        c_int,
        MRMediaRemoteGetNowPlayingApplicationIsPlaying
    );
    mr_debug!("query_is_playing -> {:?}", result);
    result
}

fn query_title_artist() -> Option<(Option<String>, Option<String>)> {
    let result = safely_dispatch_and_wait!(
        |dict: *const CFDictionary| {
            if dict.is_null() {
                mr_debug!("query_title_artist callback got null dictionary");
                return None;
            }

            unsafe {
                let dict = &*dict;
                let count = dict.count() as usize;
                mr_debug!("query_title_artist dictionary count={}", count);
                let mut keys: Vec<*const c_void> = vec![ptr::null(); count];
                let mut values: Vec<*const c_void> = vec![ptr::null(); count];
                dict.keys_and_values(keys.as_mut_ptr(), values.as_mut_ptr());

                let mut title = None;
                let mut artist = None;
                let mut all_keys = Vec::with_capacity(count);

                for i in 0..count {
                    let key_ptr = keys[i];
                    let value_ptr = values[i];
                    if key_ptr.is_null() || value_ptr.is_null() {
                        continue;
                    }

                    let key_ref = &*(key_ptr as *const NSString);
                    let key = key_ref.to_string();
                    all_keys.push(key.clone());
                    if key != TITLE_KEY && key != ARTIST_KEY {
                        continue;
                    }

                    let val_ref = &*(value_ptr as *const AnyObject);
                    let class_name = val_ref.class().name().to_str().unwrap_or_default();
                    if !matches!(
                        class_name,
                        "__NSCFString" | "__NSCFConstantString" | "NSTaggedPointerString"
                    ) {
                        mr_debug!("key {} had unsupported class {}; skipping", key, class_name);
                        continue;
                    }

                    let value = (&*(value_ptr as *const NSString)).to_string();
                    if key == TITLE_KEY {
                        title = Some(value);
                    } else if key == ARTIST_KEY {
                        artist = Some(value);
                    }
                }

                mr_debug!(
                    "query_title_artist keys={:?} parsed_title={:?} parsed_artist={:?}",
                    all_keys,
                    title,
                    artist
                );
                Some((title, artist))
            }
        },
        *const CFDictionary,
        MRMediaRemoteGetNowPlayingInfo
    );
    mr_debug!("query_title_artist -> {:?}", result);
    result
}

fn refresh_all(info: Arc<RwLock<NowPlayingInfo>>) {
    let is_playing = query_is_playing();
    let title_artist = query_title_artist();
    let mut guard = info.write().unwrap();
    let before = guard.clone();

    if let Some(is_playing) = is_playing {
        guard.is_playing = Some(is_playing);
    }
    if let Some((title, artist)) = title_artist {
        guard.title = title.and_then(|v| {
            let trimmed = v.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
        guard.artist = artist.and_then(|v| {
            let trimmed = v.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
    }

    mr_debug!("refresh_all: before={:?} after={:?}", before, &*guard);
}

fn add_observer<F: Fn() + 'static>(name: &str, closure: F) -> Observer {
    unsafe {
        let observer = NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSString::from_str(name).as_ref()),
            None,
            None,
            &RcBlock::new(move |_: NonNull<NSNotification>| closure()),
        );

        Retained::cast_unchecked(observer)
    }
}

fn remove_observer(observer: Observer) {
    unsafe {
        NSNotificationCenter::defaultCenter().removeObserver(&observer);
    }
}

impl NowPlaying {
    pub fn new() -> Self {
        let info = Arc::new(RwLock::new(NowPlayingInfo::default()));
        let mut observers = vec![];
        mr_debug!("NowPlaying::new");

        unsafe {
            let queue = media_remote_queue();
            if !queue.is_null() {
                MRMediaRemoteRegisterForNowPlayingNotifications(queue);
                mr_debug!("registered MediaRemote notifications on queue {:p}", queue);
            } else {
                mr_debug!("failed to register notifications: queue was null");
            }
        }

        refresh_all(Arc::clone(&info));

        {
            let info = Arc::clone(&info);
            observers.push(add_observer(NOTIF_INFO_CHANGE, move || {
                mr_debug!("received notification: {}", NOTIF_INFO_CHANGE);
                refresh_all(Arc::clone(&info));
            }));
        }
        {
            let info = Arc::clone(&info);
            observers.push(add_observer(NOTIF_PLAYING_CHANGE, move || {
                mr_debug!("received notification: {}", NOTIF_PLAYING_CHANGE);
                refresh_all(Arc::clone(&info));
            }));
        }
        mr_debug!("installed {} NSNotification observers", observers.len());

        Self { info, observers }
    }

    pub fn get_info(&self) -> RwLockReadGuard<'_, NowPlayingInfo> {
        self.info.read().unwrap()
    }
}

impl Drop for NowPlaying {
    fn drop(&mut self) {
        mr_debug!("NowPlaying::drop");
        while let Some(observer) = self.observers.pop() {
            remove_observer(observer);
        }

        unsafe {
            MRMediaRemoteUnregisterForNowPlayingNotifications();
            mr_debug!("unregistered MediaRemote notifications");
        }
    }
}

#[link(name = "MediaRemote", kind = "framework")]
unsafe extern "C" {
    fn MRMediaRemoteGetNowPlayingApplicationIsPlaying(
        queue: *mut dispatch_queue_s,
        block: &block2::DynBlock<dyn Fn(c_int)>,
    );

    fn MRMediaRemoteGetNowPlayingInfo(
        queue: *mut dispatch_queue_s,
        block: &block2::DynBlock<dyn Fn(*const CFDictionary)>,
    );

    fn MRMediaRemoteRegisterForNowPlayingNotifications(queue: *mut dispatch_queue_s);

    fn MRMediaRemoteUnregisterForNowPlayingNotifications();
}
