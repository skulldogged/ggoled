// This is a wrapper around `ggoled_lib` that has high-level draw functions and additional events.
// Heavily specialised for `ggoled_cli` and `ggoled_app`, and is therefore not recommended for general use.

use anyhow::bail;
use ggoled_lib::{bitmap::BitVec, Bitmap, Device, DeviceEvent};
use image::{codecs::gif::GifDecoder, AnimationDecoder, ImageFormat, ImageReader};
use rusttype::{point, Font, Scale};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

enum FontInner {
    Ttf { font: Font<'static>, size: f32 },
    Bdf { font: Box<bdf2::Font> },
}

pub struct TextRenderer {
    inner: FontInner,
}
impl TextRenderer {
    pub fn load_from_file(path: &PathBuf, size: f32) -> anyhow::Result<Self> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext.to_lowercase().as_str() {
            "bdf" => {
                let font = bdf2::open(path).map_err(|e| anyhow::anyhow!("Failed to load BDF font: {:?}", e))?;
                Ok(Self {
                    inner: FontInner::Bdf { font: Box::new(font) },
                })
            }
            _ => {
                let data = std::fs::read(path)?;
                let Some(font) = Font::try_from_vec(data) else {
                    bail!("Failed to load font");
                };
                Ok(Self {
                    inner: FontInner::Ttf { font, size },
                })
            }
        }
    }
    pub fn new_pixel_operator() -> Self {
        Self {
            inner: FontInner::Ttf {
                font: Font::try_from_bytes(include_bytes!("../fonts/PixelOperator.ttf")).unwrap(),
                size: 16.0,
            },
        }
    }
    pub fn new_cozette() -> Self {
        let font_data = include_str!("../fonts/cozette.bdf");
        let cursor = std::io::Cursor::new(font_data.as_bytes());
        Self {
            inner: FontInner::Bdf {
                font: Box::new(bdf2::read(cursor).unwrap()),
            },
        }
    }
    pub fn line_height(&self) -> usize {
        match &self.inner {
            FontInner::Ttf { font, size } => {
                let scale = Scale::uniform(*size);
                let v_metrics = font.v_metrics(scale);
                (v_metrics.ascent - v_metrics.descent).ceil() as usize
            }
            FontInner::Bdf { font } => font.bounds().height as usize,
        }
    }
    pub fn measure_line_widths(&self, text: &str) -> Vec<usize> {
        let clean_text = text.replace('\r', "");
        let text_lines = clean_text.split('\n');
        match &self.inner {
            FontInner::Ttf { font, size } => {
                let scale = Scale::uniform(*size);
                text_lines
                    .map(|text_line| {
                        let glyphs: Vec<_> = font.layout(text_line, scale, point(0.0, 0.0)).collect();
                        let mut line_w_offset = 0;
                        let mut line_w = 0;
                        for bb in glyphs.iter().filter_map(|g| g.pixel_bounding_box()) {
                            line_w_offset = line_w_offset.max(-bb.min.x);
                            line_w = line_w.max(bb.max.x + 1);
                        }
                        (line_w + line_w_offset) as usize
                    })
                    .collect()
            }
            FontInner::Bdf { font } => {
                let bounds = font.bounds();
                text_lines
                    .map(|text_line| {
                        let mut cursor_x: i32 = 0;
                        for ch in text_line.chars() {
                            if let Some(glyph) = font.glyphs().get(&ch) {
                                cursor_x += glyph.device_width().unwrap_or(&(bounds.width, 0)).0 as i32;
                            }
                        }
                        cursor_x.max(0) as usize
                    })
                    .collect()
            }
        }
    }
    pub fn render_lines(&self, text: &str) -> Vec<Bitmap> {
        let clean_text = text.replace('\r', "");
        let text_lines = clean_text.split('\n');
        match &self.inner {
            FontInner::Ttf { font, size } => {
                let scale = Scale::uniform(*size);
                text_lines
                    .map(|text_line| {
                        let glyphs: Vec<_> = font.layout(text_line, scale, point(0.0, 0.0)).collect();
                        let mut line_w_offset = 0;
                        let mut line_h_offset = 0;
                        let mut line_w = 0;
                        let mut line_h = 0;
                        for bb in glyphs.iter().filter_map(|g| g.pixel_bounding_box()) {
                            line_w_offset = line_w_offset.max(-bb.min.x);
                            line_h_offset = line_h_offset.max(-bb.min.y);
                            line_w = line_w.max(bb.max.x + 1);
                            line_h = line_h.max(bb.max.y + 1);
                        }
                        let line_w = (line_w + line_w_offset) as usize;
                        let line_h = (line_h + line_h_offset) as usize;
                        let mut bitmap = Bitmap::new(line_w, line_h, false);
                        for glyph in glyphs {
                            if let Some(bb) = glyph.pixel_bounding_box() {
                                glyph.draw(|x, y, v| {
                                    if v > 0.5 {
                                        let px = (x as i32 + line_w_offset + bb.min.x) as usize;
                                        let py = (y as i32 + line_h_offset + bb.min.y) as usize;
                                        bitmap.data.set(py * line_w + px, true);
                                    }
                                })
                            }
                        }
                        bitmap
                    })
                    .collect()
            }
            FontInner::Bdf { font } => {
                let bounds = font.bounds();
                let line_h = bounds.height as usize;
                text_lines
                    .map(|text_line| {
                        if text_line.is_empty() {
                            return Bitmap::new(0, line_h, false);
                        }
                        let mut glyph_data: Vec<(&bdf2::Glyph, i32)> = Vec::new();
                        let mut cursor_x: i32 = 0;
                        for ch in text_line.chars() {
                            if let Some(glyph) = font.glyphs().get(&ch) {
                                glyph_data.push((glyph, cursor_x));
                                cursor_x += glyph.device_width().unwrap_or(&(bounds.width, 0)).0 as i32;
                            }
                        }
                        let line_w = cursor_x.max(0) as usize;
                        if line_w == 0 {
                            return Bitmap::new(0, line_h, false);
                        }
                        let mut bitmap = Bitmap::new(line_w, line_h, false);
                        for (glyph, x_off) in &glyph_data {
                            let gb = glyph.bounds();
                            let baseline_y = bounds.height as i32 + bounds.y;
                            let glyph_origin_y = baseline_y - gb.height as i32 - gb.y;
                            let glyph_origin_x = *x_off + gb.x;
                            for gy in 0..glyph.height() as i32 {
                                for gx in 0..glyph.width() as i32 {
                                    if glyph.get(gx as u32, gy as u32) {
                                        let px = (glyph_origin_x + gx) as usize;
                                        let py = (glyph_origin_y + gy) as usize;
                                        if px < line_w && py < line_h {
                                            bitmap.data.set(py * line_w + px, true);
                                        }
                                    }
                                }
                            }
                        }
                        bitmap
                    })
                    .collect()
            }
        }
    }
}

fn bitmap_from_image(img: &image::RgbaImage, threshold: u8) -> Bitmap {
    Bitmap {
        w: img.width() as usize,
        h: img.height() as usize,
        data: img
            .pixels()
            .map(|p| (((p.0[0] as usize) + (p.0[1] as usize) + (p.0[2] as usize)) / 3) >= threshold as usize)
            .collect::<BitVec>(),
    }
}
fn bitmap_from_dynimage(img: &image::DynamicImage, threshold: u8) -> Bitmap {
    bitmap_from_image(&img.to_rgba8(), threshold)
}
pub fn bitmap_from_memory(buf: &[u8], threshold: u8) -> anyhow::Result<Bitmap> {
    let img = image::load_from_memory(buf)?;
    Ok(bitmap_from_dynimage(&img, threshold))
}

#[derive(Clone)]
pub struct Frame {
    pub bitmap: Arc<Bitmap>,
    pub delay: Option<Duration>,
}

pub fn decode_frames(path: &str, threshold: u8) -> Vec<Frame> {
    let reader = ImageReader::open(path).expect("Failed to open image");
    if matches!(reader.format().unwrap(), ImageFormat::Gif) {
        let gif = GifDecoder::new(reader.into_inner()).expect("Failed to decode gif");
        let frames = gif.into_frames();
        frames
            .map(|frame| {
                let frame = frame.expect("Failed to decode gif frame");
                let bitmap = Arc::new(bitmap_from_image(frame.buffer(), threshold));
                Frame {
                    bitmap,
                    delay: Some(Duration::from_millis(frame.delay().numer_denom_ms().0 as u64)),
                }
            })
            .collect()
    } else {
        let img = reader.decode().expect("Failed to decode image");
        let bitmap = Arc::new(bitmap_from_dynimage(&img, threshold));
        vec![Frame { bitmap, delay: None }]
    }
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Hash, Eq, Ord)]
pub struct LayerId(usize);
impl LayerId {
    pub fn none() -> LayerId {
        LayerId(0)
    }
}

pub enum DrawLayer {
    Image {
        bitmap: Arc<Bitmap>,
        x: isize,
        y: isize,
    },
    ImageNoShift {
        bitmap: Arc<Bitmap>,
        x: isize,
        y: isize,
    },
    Animation {
        frames: Vec<Frame>,
        x: isize,
        y: isize,
        follow_fps: bool,
    },
    Scroll {
        bitmap: Arc<Bitmap>,
        y: isize,
    },
    ScrollNoShift {
        bitmap: Arc<Bitmap>,
        y: isize,
    },
}

#[derive(Clone, Copy)]
pub enum TextOverflowMode {
    Scroll,
    Clip,
}

pub enum ShiftMode {
    Off,
    Simple,
}

enum DrawCommand {
    Play,
    Pause,
    SetVolume(u8),
    SetShiftMode(ShiftMode),
    Stop,
}

#[derive(Debug)]
pub enum DrawEvent {
    DeviceDisconnected,
    DeviceReconnected,
    DeviceEvent(DeviceEvent),
}

struct AnimState {
    ticks: usize,
    next_update: Instant,
}
struct ScrollState {
    x: isize,
    pause_until: Option<Instant>,
}

struct DrawLayerState {
    layer: DrawLayer,
    anim: AnimState,
    scroll: ScrollState,
}

enum RenderOp {
    Blit {
        bitmap: Arc<Bitmap>,
        x: isize,
        y: isize,
    },
    Scroll {
        bitmap: Arc<Bitmap>,
        x: isize,
        y: isize,
        scroll_w: isize,
        dupes: usize,
    },
}

const OLED_SHIFT_PERIOD: Duration = Duration::from_secs(90);
const OLED_SHIFTS: [(isize, isize); 9] = [
    (0, 0),
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

const RECONNECT_PERIOD: Duration = Duration::from_secs(1);
const SCROLL_REVOLUTION_PAUSE: Duration = Duration::from_millis(900);
const MAX_ANIM_CATCHUP_STEPS: usize = 8;

fn add_layer_to_map(layer_counter: &mut usize, layers: &mut LayerMap, layer: DrawLayer) -> LayerId {
    *layer_counter += 1;
    let id = LayerId(*layer_counter);
    _ = layers.insert(
        id,
        DrawLayerState {
            layer,
            anim: AnimState {
                ticks: 0,
                next_update: Instant::now(),
            },
            scroll: ScrollState {
                x: 0,
                pause_until: None,
            },
        },
    );
    id
}

fn add_text_layers(
    texter: &TextRenderer,
    width: usize,
    height: usize,
    layer_counter: &mut usize,
    layers: &mut LayerMap,
    text: &str,
    x: Option<isize>,
    y: Option<isize>,
    shift: bool,
    mode: TextOverflowMode,
) -> Vec<LayerId> {
    let bitmaps: Vec<_> = texter.render_lines(text).into_iter().map(Arc::new).collect();
    let line_height = texter.line_height();
    let center_y: isize = (height as isize - (line_height * bitmaps.len()) as isize) / 2;
    bitmaps
        .into_iter()
        .enumerate()
        .map(|(i, bitmap)| {
            let y = y.unwrap_or(center_y) + (i * line_height) as isize;
            let center_x = (width as isize - bitmap.w as isize) / 2;
            if bitmap.w >= width && matches!(mode, TextOverflowMode::Scroll) {
                add_layer_to_map(
                    layer_counter,
                    layers,
                    if shift {
                        DrawLayer::Scroll { bitmap, y }
                    } else {
                        DrawLayer::ScrollNoShift { bitmap, y }
                    },
                )
            } else {
                add_layer_to_map(
                    layer_counter,
                    layers,
                    if shift {
                        DrawLayer::Image {
                            bitmap,
                            x: x.unwrap_or(center_x),
                            y,
                        }
                    } else {
                        DrawLayer::ImageNoShift {
                            bitmap,
                            x: x.unwrap_or(center_x),
                            y,
                        }
                    },
                )
            }
        })
        .collect()
}

fn normalize_anim_delay(delay: Option<Duration>, fallback_delay: Duration) -> Duration {
    let delay = delay.unwrap_or(fallback_delay);
    if delay.is_zero() {
        fallback_delay
    } else {
        delay
    }
}

fn advance_animation_state(
    anim: &mut AnimState,
    frames: &[Frame],
    follow_fps: bool,
    now: Instant,
    fallback_delay: Duration,
) -> Option<usize> {
    if frames.is_empty() {
        return None;
    }
    let render_idx = anim.ticks % frames.len();
    if follow_fps {
        anim.ticks = anim.ticks.wrapping_add(1);
        return Some(render_idx);
    }

    if now >= anim.next_update {
        let mut steps = 0;
        while now >= anim.next_update && steps < MAX_ANIM_CATCHUP_STEPS {
            let idx = anim.ticks % frames.len();
            let delay = normalize_anim_delay(frames[idx].delay, fallback_delay);
            anim.ticks = anim.ticks.wrapping_add(1);
            anim.next_update += delay;
            steps += 1;
        }
        if now >= anim.next_update {
            anim.next_update = now + fallback_delay;
        }
    }
    Some(render_idx)
}

fn run_draw_device_thread(
    mut dev: Device,
    layers: Arc<Mutex<LayerMap>>,
    cmd_receiver: Receiver<DrawCommand>,
    event_sender: Sender<DrawEvent>,
    fps: usize,
) -> Device {
    let frame_delay = Duration::from_nanos(1_000_000_000 / fps as u64);
    let mut prev_screen = Bitmap::new(0, 0, false);
    let mut playing = false;
    let mut oled_shift = 0;
    let mut last_shift = Instant::now();
    let mut shift_mode = ShiftMode::Off;
    let mut connected = true;
    let mut last_connect_attempt = Instant::now();
    let mut last_frame_time = Instant::now();
    loop {
        let time = Instant::now();
        let mut stop_after_frame = false;
        while let Ok(cmd) = cmd_receiver.try_recv() {
            match cmd {
                DrawCommand::Play => playing = true,
                DrawCommand::Pause => playing = false,
                DrawCommand::SetVolume(volume) => {
                    if connected && dev.set_volume(volume).is_err() {
                        connected = false;
                        event_sender.send(DrawEvent::DeviceDisconnected).unwrap();
                    }
                }
                DrawCommand::SetShiftMode(mode) => shift_mode = mode,
                DrawCommand::Stop => stop_after_frame = true,
            }
        }

        // Attempt to reconnect
        if !connected && time.duration_since(last_connect_attempt) >= RECONNECT_PERIOD {
            last_connect_attempt = time;
            if dev.reconnect().is_ok() {
                connected = true;
                event_sender.send(DrawEvent::DeviceReconnected).unwrap();
            }
        }

        // Render frame
        if connected && playing {
            // Handle OLED shifts
            let (shift_x, shift_y) = match shift_mode {
                ShiftMode::Off => (0, 0),
                ShiftMode::Simple => {
                    if time.duration_since(last_shift) >= OLED_SHIFT_PERIOD {
                        oled_shift = (oled_shift + 1) % OLED_SHIFTS.len();
                        last_shift = time;
                    }
                    OLED_SHIFTS[oled_shift]
                }
            };

            let mut render_ops = vec![];
            {
                let mut layers = layers.lock().unwrap();
                render_ops.reserve(layers.len());
                for state in layers.values_mut() {
                    match &state.layer {
                        DrawLayer::Image { bitmap, x, y } => render_ops.push(RenderOp::Blit {
                            bitmap: bitmap.clone(),
                            x: x + shift_x,
                            y: y + shift_y,
                        }),
                        DrawLayer::ImageNoShift { bitmap, x, y } => render_ops.push(RenderOp::Blit {
                            bitmap: bitmap.clone(),
                            x: *x,
                            y: *y,
                        }),
                        DrawLayer::Animation {
                            frames,
                            x,
                            y,
                            follow_fps,
                        } => {
                            if let Some(frame_idx) =
                                advance_animation_state(&mut state.anim, frames, *follow_fps, time, frame_delay)
                            {
                                render_ops.push(RenderOp::Blit {
                                    bitmap: frames[frame_idx].bitmap.clone(),
                                    x: x + shift_x,
                                    y: y + shift_y,
                                });
                            }
                        }
                        DrawLayer::Scroll { bitmap, y } => {
                            const MARGIN: isize = 30;
                            let scroll_w = bitmap.w as isize + MARGIN;
                            let dupes = 1 + dev.width / scroll_w as usize;
                            render_ops.push(RenderOp::Scroll {
                                bitmap: bitmap.clone(),
                                x: state.scroll.x + shift_x,
                                y: *y + shift_y,
                                scroll_w,
                                dupes,
                            });
                            let paused = state.scroll.pause_until.is_some_and(|until| time < until);
                            if !paused {
                                state.scroll.x -= 1;
                                if state.scroll.x <= -scroll_w {
                                    state.scroll.x += scroll_w;
                                    state.scroll.pause_until = Some(time + SCROLL_REVOLUTION_PAUSE);
                                }
                            } else if state.scroll.pause_until.is_some_and(|until| time >= until) {
                                state.scroll.pause_until = None;
                            }
                        }
                        DrawLayer::ScrollNoShift { bitmap, y } => {
                            const MARGIN: isize = 30;
                            let scroll_w = bitmap.w as isize + MARGIN;
                            let dupes = 1 + dev.width / scroll_w as usize;
                            render_ops.push(RenderOp::Scroll {
                                bitmap: bitmap.clone(),
                                x: state.scroll.x,
                                y: *y,
                                scroll_w,
                                dupes,
                            });
                            let paused = state.scroll.pause_until.is_some_and(|until| time < until);
                            if !paused {
                                state.scroll.x -= 1;
                                if state.scroll.x <= -scroll_w {
                                    state.scroll.x += scroll_w;
                                    state.scroll.pause_until = Some(time + SCROLL_REVOLUTION_PAUSE);
                                }
                            } else if state.scroll.pause_until.is_some_and(|until| time >= until) {
                                state.scroll.pause_until = None;
                            }
                        }
                    }
                }
            }

            let mut screen = Bitmap::new(dev.width, dev.height, false);
            for op in render_ops {
                match op {
                    RenderOp::Blit { bitmap, x, y } => screen.blit(&bitmap, x, y, false),
                    RenderOp::Scroll {
                        bitmap,
                        x,
                        y,
                        scroll_w,
                        dupes,
                    } => {
                        for i in 0..=dupes {
                            screen.blit(&bitmap, x + i as isize * scroll_w, y, false);
                        }
                    }
                }
            }

            // Draw update
            let frame_time = Instant::now();
            let force_redraw = frame_time.duration_since(last_frame_time) >= Duration::from_secs(1);
            if screen != prev_screen || force_redraw {
                last_frame_time = frame_time;
                if let Err(_err) = dev.draw(&screen, 0, 0) {
                    if connected {
                        connected = false;
                        event_sender.send(DrawEvent::DeviceDisconnected).unwrap();
                    }
                } else {
                    prev_screen = screen;
                }
            }
        }

        // Get device events and pass back to DrawDevice
        if connected {
            let events = dev.get_events().unwrap_or_else(|_| {
                connected = false;
                event_sender.send(DrawEvent::DeviceDisconnected).unwrap();
                vec![]
            });
            for event in events {
                event_sender.send(DrawEvent::DeviceEvent(event)).unwrap();
            }
        }

        // Stop
        if stop_after_frame {
            break;
        }

        // Delay as long as needed based on how long frame rendering took (which will mostly depend on USB speed)
        let frame_duration = Instant::now().duration_since(time);
        // println!("frame: {:?}, {:?}", frame_duration, frame_delay);
        spin_sleep::sleep(frame_delay.saturating_sub(frame_duration));
    }
    dev
}

type LayerMap = BTreeMap<LayerId, DrawLayerState>;
pub struct DrawDevice {
    width: usize,
    height: usize,
    layers: Arc<Mutex<LayerMap>>,
    layer_counter: usize,
    thread: Option<std::thread::JoinHandle<Device>>,
    cmd_sender: Sender<DrawCommand>,
    event_receiver: Receiver<DrawEvent>,
    pub texter: TextRenderer,
}

pub struct LayerTxn<'a> {
    width: usize,
    height: usize,
    layer_counter: &'a mut usize,
    layers: MutexGuard<'a, LayerMap>,
    texter: &'a TextRenderer,
}
impl<'a> LayerTxn<'a> {
    pub fn add_layer(&mut self, layer: DrawLayer) -> LayerId {
        add_layer_to_map(self.layer_counter, &mut self.layers, layer)
    }
    pub fn remove_layer(&mut self, id: LayerId) {
        self.layers.remove(&id);
    }
    pub fn remove_layers(&mut self, ids: &[LayerId]) {
        for id in ids {
            self.layers.remove(id);
        }
    }
    pub fn clear_layers(&mut self) {
        self.layers.clear();
    }
    pub fn add_text_with_mode(
        &mut self,
        text: &str,
        x: Option<isize>,
        y: Option<isize>,
        shift: bool,
        mode: TextOverflowMode,
    ) -> Vec<LayerId> {
        add_text_layers(
            self.texter,
            self.width,
            self.height,
            self.layer_counter,
            &mut self.layers,
            text,
            x,
            y,
            shift,
            mode,
        )
    }
}

impl DrawDevice {
    pub fn new(dev: Device, fps: usize) -> DrawDevice {
        let layers: Arc<Mutex<LayerMap>> = Default::default();
        let (cmd_sender, cmd_recver) = channel::<DrawCommand>();
        let (event_sender, event_receiver) = channel::<DrawEvent>();
        let c_layers = layers.clone();
        let (width, height) = (dev.width, dev.height);
        let thread = Some(std::thread::spawn(move || {
            run_draw_device_thread(dev, c_layers, cmd_recver, event_sender, fps)
        }));
        DrawDevice {
            width,
            height,
            layers,
            layer_counter: 0,
            thread,
            cmd_sender,
            event_receiver,
            texter: TextRenderer::new_pixel_operator(),
        }
    }
    fn destroy(&mut self) -> Option<Device> {
        if let Some(thread) = self.thread.take() {
            self.cmd_sender.send(DrawCommand::Stop).unwrap();
            Some(thread.join().unwrap())
        } else {
            None
        }
    }
    pub fn stop(mut self) -> Device {
        self.destroy().unwrap()
    }
    pub fn try_event(&mut self) -> Option<DrawEvent> {
        self.event_receiver.try_recv().ok()
    }
    pub fn poll_event(&mut self) -> DrawEvent {
        self.event_receiver.recv().unwrap()
    }
    pub fn center_bitmap(&self, bitmap: &Bitmap) -> (isize, isize) {
        (
            (self.width as isize - bitmap.w as isize) / 2,
            (self.height as isize - bitmap.h as isize) / 2,
        )
    }
    pub fn add_layer(&mut self, layer: DrawLayer) -> LayerId {
        let mut layers = self.layers.lock().unwrap();
        add_layer_to_map(&mut self.layer_counter, &mut layers, layer)
    }
    pub fn remove_layer(&mut self, id: LayerId) {
        self.layers.lock().unwrap().remove(&id);
    }
    pub fn remove_layers(&mut self, ids: &[LayerId]) {
        let mut layers = self.layers.lock().unwrap();
        for id in ids {
            layers.remove(id);
        }
    }
    pub fn clear_layers(&mut self) {
        self.layers.lock().unwrap().clear();
    }
    pub fn font_line_height(&self) -> usize {
        self.texter.line_height()
    }
    pub fn measure_line_widths(&self, text: &str) -> Vec<usize> {
        self.texter.measure_line_widths(text)
    }
    pub fn transact_layers<R>(&mut self, f: impl FnOnce(&mut LayerTxn<'_>) -> R) -> R {
        let layers = self.layers.lock().unwrap();
        let mut txn = LayerTxn {
            width: self.width,
            height: self.height,
            layer_counter: &mut self.layer_counter,
            layers,
            texter: &self.texter,
        };
        f(&mut txn)
    }
    pub fn add_text_with_mode(
        &mut self,
        text: &str,
        x: Option<isize>,
        y: Option<isize>,
        shift: bool,
        mode: TextOverflowMode,
    ) -> Vec<LayerId> {
        let mut layers = self.layers.lock().unwrap();
        add_text_layers(
            &self.texter,
            self.width,
            self.height,
            &mut self.layer_counter,
            &mut layers,
            text,
            x,
            y,
            shift,
            mode,
        )
    }
    pub fn add_text(&mut self, text: &str, x: Option<isize>, y: Option<isize>) -> Vec<LayerId> {
        self.add_text_with_mode(text, x, y, true, TextOverflowMode::Scroll)
    }
    pub fn add_text_no_shift(&mut self, text: &str, x: Option<isize>, y: Option<isize>) -> Vec<LayerId> {
        self.add_text_with_mode(text, x, y, false, TextOverflowMode::Scroll)
    }
    pub fn set_shift_mode(&mut self, mode: ShiftMode) {
        self.cmd_sender.send(DrawCommand::SetShiftMode(mode)).unwrap();
    }
    pub fn set_volume(&mut self, volume: u8) {
        self.cmd_sender.send(DrawCommand::SetVolume(volume)).unwrap();
    }
    pub fn play(&mut self) {
        self.cmd_sender.send(DrawCommand::Play).unwrap();
    }
    pub fn pause(&mut self) {
        self.cmd_sender.send(DrawCommand::Pause).unwrap();
    }
}
impl Drop for DrawDevice {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(delay: Option<Duration>) -> Frame {
        Frame {
            bitmap: Arc::new(Bitmap::new(1, 1, true)),
            delay,
        }
    }

    #[test]
    fn advance_animation_clamps_zero_delay() {
        let now = Instant::now();
        let mut anim = AnimState {
            ticks: 0,
            next_update: now - Duration::from_millis(1),
        };
        let frames = vec![frame(Some(Duration::ZERO))];
        let fallback = Duration::from_millis(33);
        let render_idx = advance_animation_state(&mut anim, &frames, false, now, fallback);
        assert_eq!(render_idx, Some(0));
        assert_eq!(anim.ticks, 1);
        assert!(anim.next_update > now);
    }

    #[test]
    fn advance_animation_catches_up_when_behind() {
        let now = Instant::now();
        let mut anim = AnimState {
            ticks: 0,
            next_update: now - Duration::from_millis(35),
        };
        let frames = vec![
            frame(Some(Duration::from_millis(10))),
            frame(Some(Duration::from_millis(10))),
        ];
        let render_idx = advance_animation_state(&mut anim, &frames, false, now, Duration::from_millis(33));
        assert_eq!(render_idx, Some(0));
        assert!(anim.ticks >= 2);
        assert!(anim.ticks <= MAX_ANIM_CATCHUP_STEPS);
        assert!(anim.next_update > now);
    }

    #[test]
    fn advance_animation_bounds_catchup_steps() {
        let now = Instant::now();
        let mut anim = AnimState {
            ticks: 0,
            next_update: now - Duration::from_secs(10),
        };
        let frames = vec![frame(Some(Duration::ZERO))];
        let fallback = Duration::from_millis(16);
        let _ = advance_animation_state(&mut anim, &frames, false, now, fallback);
        assert_eq!(anim.ticks, MAX_ANIM_CATCHUP_STEPS);
        assert!(anim.next_update >= now + fallback);
    }
}
