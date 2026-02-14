use super::{Media, PlatformCapabilities};
use std::mem::size_of;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager, GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows_sys::Win32::{
    System::SystemInformation::GetTickCount,
    UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APP_NAME: &str = "GGOLED";

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        media: true,
        idle_timeout: true,
        autostart: true,
    }
}

pub fn set_autostart(enabled: bool) {
    use windows_sys::Win32::System::Registry::{
        RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_WRITE, REG_SZ,
    };

    unsafe {
        let mut hkey: *mut std::ffi::c_void = std::ptr::null_mut();
        let subkey: Vec<u16> = RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect();

        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_WRITE, &mut hkey) == 0 {
            if enabled {
                let exe_path = std::env::current_exe().unwrap();
                let exe_str = exe_path.to_string_lossy().to_string();
                let exe_wide: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();
                let app_name_wide: Vec<u16> = APP_NAME.encode_utf16().chain(std::iter::once(0)).collect();

                RegSetValueExW(
                    hkey,
                    app_name_wide.as_ptr(),
                    0,
                    REG_SZ,
                    exe_wide.as_ptr() as *const u8,
                    (exe_wide.len() * 2) as u32,
                );
            } else {
                let app_name_wide: Vec<u16> = APP_NAME.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = RegDeleteValueW(hkey, app_name_wide.as_ptr());
            }
        }
    }
}

pub fn get_autostart() -> bool {
    use windows_sys::Win32::System::Registry::{RegOpenKeyExW, RegQueryValueExW, HKEY_CURRENT_USER, KEY_READ, REG_SZ};

    unsafe {
        let mut hkey: *mut std::ffi::c_void = std::ptr::null_mut();
        let subkey: Vec<u16> = RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect();
        let app_name_wide: Vec<u16> = APP_NAME.encode_utf16().chain(std::iter::once(0)).collect();

        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) == 0 {
            let mut data: [u8; 1024] = [0; 1024];
            let mut data_size: u32 = 1024;
            let mut data_type: u32 = 0;

            if RegQueryValueExW(
                hkey,
                app_name_wide.as_ptr(),
                std::ptr::null_mut(),
                &mut data_type,
                data.as_mut_ptr(),
                &mut data_size,
            ) == 0
            {
                return data_type == REG_SZ && data_size > 0;
            }
        }
        false
    }
}

pub struct MediaControl {
    mgr: Option<GlobalSystemMediaTransportControlsSessionManager>,
}
impl MediaControl {
    pub fn new() -> MediaControl {
        let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map(|req| req.join().ok())
            .ok()
            .flatten();
        MediaControl { mgr }
    }
    pub fn get_media(&self, include_paused: bool) -> Option<Media> {
        if let Some(mgr) = &self.mgr {
            (|| {
                let session = mgr.GetCurrentSession()?;
                let status = session.GetPlaybackInfo()?.PlaybackStatus()?;
                let allowed = status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
                    || (include_paused && status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused);
                if allowed {
                    let request = session.TryGetMediaPropertiesAsync()?;
                    let media = request.join()?;
                    anyhow::Ok(Some(Media {
                        title: media.Title()?.to_string_lossy(),
                        artist: media.Artist()?.to_string_lossy(),
                    }))
                } else {
                    anyhow::Ok(None)
                }
            })()
            .ok()
            .flatten()
        } else {
            None
        }
    }
}

pub fn get_idle_seconds() -> usize {
    unsafe {
        let mut lastinput = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut lastinput) != 0 {
            ((GetTickCount() - lastinput.dwTime) / 1000) as usize
        } else {
            0
        }
    }
}
