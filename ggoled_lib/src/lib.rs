pub mod bitmap;
use anyhow::bail;
pub use bitmap::Bitmap;
use hidapi::{HidApi, HidDevice, MAX_REPORT_DESCRIPTOR_SIZE};
use std::{cmp::min, time::Duration};

// NOTE: these work for Arctis Nova Pro but might not for different products!
const SCREEN_REPORT_SPLIT_SZ: usize = 64;
const SCREEN_REPORT_SIZE: usize = 1024;
const BASE_STATION_VOLUME_MAX: u8 = 0x38;
const DEVICE_WIDTH: usize = 128;
const DEVICE_HEIGHT: usize = 64;

type DrawReport = [u8; SCREEN_REPORT_SIZE];

struct ReportDrawable<'a> {
    bitmap: &'a Bitmap,
    w: usize,
    h: usize,
    dst_x: usize,
    dst_y: usize,
    src_x: usize,
    src_y: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReportDrawableSpec {
    w: usize,
    h: usize,
    dst_x: usize,
    dst_y: usize,
    src_x: usize,
    src_y: usize,
}

fn prepare_drawables_for_report(
    bitmap: &Bitmap,
    x: isize,
    y: isize,
    screen_w: usize,
    screen_h: usize,
) -> Vec<ReportDrawableSpec> {
    let dst_x_start = x.max(0).min(screen_w as isize);
    let dst_y_start = y.max(0).min(screen_h as isize);
    let dst_x_end = (x + bitmap.w as isize).max(0).min(screen_w as isize);
    let dst_y_end = (y + bitmap.h as isize).max(0).min(screen_h as isize);
    if dst_x_start >= dst_x_end || dst_y_start >= dst_y_end {
        return Vec::new();
    }

    let dst_x_start = dst_x_start as usize;
    let dst_y_start = dst_y_start as usize;
    let w = (dst_x_end - dst_x_start as isize) as usize;
    let h = (dst_y_end - dst_y_start as isize) as usize;
    let src_x = (dst_x_start as isize - x) as usize;
    let src_y = (dst_y_start as isize - y) as usize;

    let mut drawables = Vec::with_capacity(w.div_ceil(SCREEN_REPORT_SPLIT_SZ));
    let splits = w.div_ceil(SCREEN_REPORT_SPLIT_SZ);
    for i in 0..splits {
        let chunk_w = min(SCREEN_REPORT_SPLIT_SZ, w - i * SCREEN_REPORT_SPLIT_SZ);
        drawables.push(ReportDrawableSpec {
            w: chunk_w,
            h,
            dst_x: dst_x_start + (i * SCREEN_REPORT_SPLIT_SZ),
            dst_y: dst_y_start,
            src_x: src_x + i * SCREEN_REPORT_SPLIT_SZ,
            src_y,
        });
    }
    drawables
}

fn create_report_for_drawable(bitmap: &Bitmap, d: ReportDrawableSpec) -> DrawReport {
    let mut report: DrawReport = [0; SCREEN_REPORT_SIZE];
    report[0] = 0x06; // hid report id
    report[1] = 0x93; // command id
    report[2] = d.dst_x as u8;
    report[3] = d.dst_y as u8;
    report[4] = d.w as u8;
    report[5] = d.h as u8;
    let stride_h = (d.dst_y.wrapping_rem(8) + d.h).div_ceil(8) * 8;
    for y in 0..d.h {
        for x in 0..d.w {
            // NOTE: report has columns rather than rows
            let ri = x * stride_h + y;
            let pi = (d.src_y + y) * bitmap.w + (d.src_x + x);
            let report_i = (ri / 8) + 6;
            debug_assert!(report_i < SCREEN_REPORT_SIZE);
            report[report_i] |= (bitmap.data[pi] as u8) << (ri % 8);
        }
    }
    report
}

#[derive(Debug)]
pub enum DeviceEvent {
    Volume {
        volume: u8,
    },
    Battery {
        headset: u8,
        charging: u8,
    },
    HeadsetConnection {
        wireless: bool,
        bluetooth: bool,
        bluetooth_on: bool,
    },
}

pub struct Device {
    oled_dev: HidDevice,
    info_dev: Option<HidDevice>,
    info_blocking_mode: Option<bool>,
    pub width: usize,
    pub height: usize,
}
impl Device {
    /// Connect to a SteelSeries GG device.
    pub fn connect() -> anyhow::Result<Device> {
        let api = HidApi::new().unwrap();

        // Find all connected devices matching given Vendor/Product IDs and interface
        let device_infos: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == 0x1038 // SteelSeries
        && [
            0x12cb, // Arctis Nova Pro Wired
            0x12cd, // Arctis Nova Pro Wired (Xbox)
            0x12e0, // Arctis Nova Pro Wireless
            0x12e5, // Arctis Nova Pro Wireless (Xbox)
            0x225d, // Arctis Nova Pro Wireless (Xbox White)
        ].contains(&d.product_id()) && d.interface_number() == 4
            })
            .collect();

        // On some platforms this can be duplicated or collapsed, so we only require at least one candidate.
        if device_infos.is_empty() {
            bail!("No matching devices connected");
        }

        // If all entries point to the same path, use one handle for drawing and best-effort second handle for events.
        let all_same_path = device_infos.iter().all(|d| d.path() == device_infos[0].path());

        let (oled_dev, info_dev) = if all_same_path {
            let oled_dev = device_infos[0]
                .open_device(&api)
                .map_err(|err| anyhow::anyhow!("Failed to connect to USB device: {err}"))?;
            let info_dev = match device_infos[0].open_device(&api) {
                Ok(dev) => Some(dev),
                Err(err) => {
                    eprintln!(
                        "warning: failed to open second handle for shared HID path (falling back to single handle): {err}"
                    );
                    None
                }
            };
            (oled_dev, info_dev)

        // On platforms exposing separate interfaces, pick OLED by descriptor and best-effort select an info interface.
        } else {
            // Open all candidates
            let mut devices = device_infos
                .iter()
                .map(|info| anyhow::Ok(info.open_device(&api)?))
                .collect::<anyhow::Result<Vec<_>>>()
                .map_err(|err| anyhow::anyhow!("Failed to connect to USB device: {err}"))?;

            // Get descriptors
            let Ok(mut device_reports) = devices
                .iter()
                .map(|dev| {
                    let mut buf = [0u8; MAX_REPORT_DESCRIPTOR_SIZE];
                    let sz = dev.get_report_descriptor(&mut buf)?;
                    anyhow::Ok(Vec::from(&buf[..sz]))
                })
                .collect::<anyhow::Result<Vec<_>>>()
            else {
                bail!("Failed to get USB device HID reports");
            };

            // Identify OLED endpoint by descriptor.
            let Some(oled_dev_idx) = device_reports.iter().position(|desc| desc.get(1) == Some(&0xc0)) else {
                bail!("No OLED device found");
            };
            _ = device_reports.swap_remove(oled_dev_idx);
            let oled_dev = devices.swap_remove(oled_dev_idx);

            // Prefer known info descriptor (0x00), otherwise fallback to any non-OLED descriptor.
            let info_dev = if let Some(info_dev_idx) = device_reports.iter().position(|desc| desc.get(1) == Some(&0x00))
            {
                Some(devices.swap_remove(info_dev_idx))
            } else if let Some(fallback_idx) = device_reports.iter().position(|desc| desc.get(1) != Some(&0xc0)) {
                eprintln!("warning: using fallback HID interface for device events");
                Some(devices.swap_remove(fallback_idx))
            } else {
                eprintln!("warning: no separate HID event interface detected, using single-handle mode");
                None
            };

            (oled_dev, info_dev)
        };

        Ok(Device {
            oled_dev,
            info_dev,
            info_blocking_mode: None,
            width: DEVICE_WIDTH,
            height: DEVICE_HEIGHT,
        })
    }

    /// Dump the full device tree info for all SteelSeries devices to stdout for debug purposes
    pub fn dump_devices() {
        let api = HidApi::new().unwrap();

        let device_infos: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == 0x1038) // SteelSeries
            .collect();
        if device_infos.is_empty() {
            println!("No devices.");
            return;
        }

        println!("-----");
        for info in device_infos {
            println!("product={}", info.product_string().unwrap_or("?"));
            println!("pid={:#04x}", info.product_id());
            println!("interface={}", info.interface_number());
            println!("path={}", info.path().to_string_lossy());
            println!("usage={}", info.usage());
            if let Ok(dev) = info.open_device(&api) {
                let mut buf = [0u8; MAX_REPORT_DESCRIPTOR_SIZE];
                if let Ok(sz) = dev.get_report_descriptor(&mut buf) {
                    println!("report desc sz={sz}, first 16 bytes: {:02x?}", &buf[0..16]);
                } else {
                    println!("getting report descriptor failed");
                }
            } else {
                println!("opening device failed");
            }
            println!("-----");
        }
    }

    /// Reconnect to a device.
    pub fn reconnect(&mut self) -> anyhow::Result<()> {
        *self = Self::connect()?;
        Ok(())
    }

    // Creates a HID report for a `ReportDrawable`
    // The Bitmap must already be within the report limits (from `split_for_report`)
    fn create_report(&self, d: &ReportDrawable) -> DrawReport {
        create_report_for_drawable(
            d.bitmap,
            ReportDrawableSpec {
                w: d.w,
                h: d.h,
                dst_x: d.dst_x,
                dst_y: d.dst_y,
                src_x: d.src_x,
                src_y: d.src_y,
            },
        )
    }

    // Splits up a `Bitmap` to be appropriately sized for being able to send over USB HID
    fn prepare_for_report<'a>(&self, bitmap: &'a Bitmap, x: isize, y: isize) -> Vec<ReportDrawable<'a>> {
        prepare_drawables_for_report(bitmap, x, y, self.width, self.height)
            .into_iter()
            .map(|spec| ReportDrawable {
                bitmap,
                w: spec.w,
                h: spec.h,
                dst_x: spec.dst_x,
                dst_y: spec.dst_y,
                src_x: spec.src_x,
                src_y: spec.src_y,
            })
            .collect()
    }

    /// Draw a `Bitmap` at the given location.
    pub fn draw(&self, bitmap: &Bitmap, x: isize, y: isize) -> anyhow::Result<()> {
        let drawables = self.prepare_for_report(bitmap, x, y);
        for drawable in drawables {
            let report = self.create_report(&drawable);
            self.retry_report(&report)?;
        }
        Ok(())
    }

    fn retry_report(&self, data: &[u8]) -> anyhow::Result<()> {
        let mut i: u64 = 0;
        loop {
            match self.oled_dev.send_feature_report(data) {
                Ok(_) => return Ok(()),
                Err(err) => {
                    if i == 10 {
                        return Err(err.into());
                    }
                    i += 1;
                    spin_sleep::sleep(Duration::from_millis(i.pow(2)));
                }
            }
        }
    }

    /// Set screen brightness.
    pub fn set_brightness(&self, value: u8) -> anyhow::Result<()> {
        if value < 0x01 {
            bail!("brightness too low");
        } else if value > 0x0a {
            bail!("brightness too high");
        }
        let mut report = [0; 64];
        report[0] = 0x06; // hid report id
        report[1] = 0x85; // command id
        report[2] = value;
        self.oled_dev.write(&report)?;
        Ok(())
    }

    /// Set base station volume where `0` is mute and `56` is max volume.
    pub fn set_volume(&self, value: u8) -> anyhow::Result<()> {
        if value > BASE_STATION_VOLUME_MAX {
            bail!("volume too high");
        }
        let mut report = [0; 64];
        report[0] = 0x06; // hid report id
        report[1] = 0x25; // command id
        report[2] = BASE_STATION_VOLUME_MAX.saturating_sub(value);
        self.oled_dev.write(&report)?;
        Ok(())
    }

    /// Return to SteelSeries UI.
    pub fn return_to_ui(&self) -> anyhow::Result<()> {
        let mut report = [0; 64];
        report[0] = 0x06; // hid report id
        report[1] = 0x95; // command id
        self.oled_dev.write(&report)?;
        Ok(())
    }

    fn parse_event(buf: &[u8; 64]) -> Option<DeviceEvent> {
        #[cfg(debug_assertions)]
        println!("parse_event: {:x?}", buf);
        if buf[0] != 7 {
            return None;
        }
        Some(match buf[1] {
            0x25 => DeviceEvent::Volume {
                volume: BASE_STATION_VOLUME_MAX.saturating_sub(buf[2]),
            },
            0xb5 => DeviceEvent::HeadsetConnection {
                wireless: buf[4] == 8,
                bluetooth: buf[3] == 1,
                bluetooth_on: buf[2] == 4,
            },
            0xb7 => DeviceEvent::Battery {
                headset: buf[2],
                charging: buf[3],
                // NOTE: there's a chance `buf[4]` represents either the max value or simply just `8` for connected
            },
            _ => return None,
        })
    }

    fn set_info_blocking_mode(&mut self, blocking: bool) -> anyhow::Result<()> {
        let Some(info_dev) = self.info_dev.as_ref() else {
            return Ok(());
        };
        if self.info_blocking_mode != Some(blocking) {
            info_dev.set_blocking_mode(blocking)?;
            self.info_blocking_mode = Some(blocking);
        }
        Ok(())
    }

    /// Poll events from the device. This blocks until an event is returned.
    pub fn poll_event(&mut self) -> anyhow::Result<Option<DeviceEvent>> {
        if self.info_dev.is_none() {
            return Ok(None);
        }
        self.set_info_blocking_mode(true)?;
        let Some(info_dev) = self.info_dev.as_ref() else {
            return Ok(None);
        };
        let mut buf = [0u8; 64];
        _ = info_dev.read(&mut buf)?;
        Ok(Self::parse_event(&buf))
    }

    /// Return any pending events from the device. Non-blocking.
    pub fn get_events(&mut self) -> anyhow::Result<Vec<DeviceEvent>> {
        if self.info_dev.is_none() {
            return Ok(vec![]);
        }
        self.set_info_blocking_mode(false)?;
        let Some(info_dev) = self.info_dev.as_ref() else {
            return Ok(vec![]);
        };
        let mut events = Vec::with_capacity(4);
        loop {
            let mut buf = [0u8; 64];
            let len = info_dev.read(&mut buf)?;
            if len == 0 {
                break;
            } else if let Some(event) = Self::parse_event(&buf) {
                events.push(event);
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prepare_drawables_and_reports_are_bounds_safe(
            bitmap_w in 0usize..256,
            bitmap_h in 0usize..128,
            x in -256isize..256,
            y in -128isize..128
        ) {
            let bitmap = Bitmap::new(bitmap_w, bitmap_h, true);
            let drawables = prepare_drawables_for_report(&bitmap, x, y, DEVICE_WIDTH, DEVICE_HEIGHT);
            for d in drawables {
                prop_assert!(d.w <= SCREEN_REPORT_SPLIT_SZ);
                prop_assert!(d.dst_x < DEVICE_WIDTH);
                prop_assert!(d.dst_y < DEVICE_HEIGHT);
                prop_assert!(d.src_x <= bitmap_w);
                prop_assert!(d.src_y <= bitmap_h);
                prop_assert!(d.src_x + d.w <= bitmap_w);
                prop_assert!(d.src_y + d.h <= bitmap_h);
                prop_assert!(d.dst_x + d.w <= DEVICE_WIDTH);
                prop_assert!(d.dst_y + d.h <= DEVICE_HEIGHT);
                let _ = create_report_for_drawable(&bitmap, d);
            }
        }
    }
}
