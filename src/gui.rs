use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Margin, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2,
};

use windows_capture::monitor::Monitor;
use windows_capture::window::Window;

use crate::history::{self, HistoryEntry};
use crate::hotkey::{HotkeyAction, HotkeyListener};
use crate::icons::{self, Icon};
use crate::monitor_util::{self, MonitorBounds};
use crate::recorder::{self, ActiveRecording, CaptureTarget, Codec, RecordingOptions, Region};
use crate::util;
use crate::webcam::{self as camera, Corner, WebcamOptions};

const BG: Color32 = Color32::from_rgb(10, 12, 16);
const PANEL_TOP: Color32 = Color32::from_rgb(15, 18, 24);
const SURFACE: Color32 = Color32::from_rgb(20, 23, 30);
const SURFACE_ELEVATED: Color32 = Color32::from_rgb(25, 29, 38);
const SURFACE_HOVER: Color32 = Color32::from_rgb(30, 35, 45);
const SURFACE_ACTIVE: Color32 = Color32::from_rgb(34, 40, 52);
const INPUT_BG: Color32 = Color32::from_rgb(13, 16, 21);
const BORDER: Color32 = Color32::from_rgb(38, 44, 56);
const BORDER_SOFT: Color32 = Color32::from_rgb(31, 36, 46);
const BORDER_HI: Color32 = Color32::from_rgb(58, 67, 84);
const TEXT: Color32 = Color32::from_rgb(236, 239, 245);
const TEXT_SOFT: Color32 = Color32::from_rgb(197, 204, 216);
const MUTED: Color32 = Color32::from_rgb(145, 155, 173);
const MUTED_DIM: Color32 = Color32::from_rgb(105, 114, 131);
const ACCENT: Color32 = Color32::from_rgb(79, 124, 255);
const ACCENT_HI: Color32 = Color32::from_rgb(112, 150, 255);
const ACCENT_SOFT: Color32 = Color32::from_rgb(25, 39, 76);
const DANGER: Color32 = Color32::from_rgb(229, 72, 77);
const DANGER_HI: Color32 = Color32::from_rgb(244, 92, 96);
const DANGER_SOFT: Color32 = Color32::from_rgb(54, 25, 29);
const DANGER_BORDER: Color32 = Color32::from_rgb(105, 42, 47);
const SUCCESS: Color32 = Color32::from_rgb(63, 201, 110);
const SUCCESS_SOFT: Color32 = Color32::from_rgb(21, 47, 31);
const CAUTION: Color32 = Color32::from_rgb(240, 184, 76);
const CAUTION_SOFT: Color32 = Color32::from_rgb(52, 40, 20);

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetMode {
    Monitor,
    Window,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quality {
    Low,
    Balanced,
    High,
    Ultra,
}

impl Quality {
    const ALL: [Quality; 4] = [
        Quality::Low,
        Quality::Balanced,
        Quality::High,
        Quality::Ultra,
    ];

    fn label(self) -> &'static str {
        match self {
            Quality::Low => "Low",
            Quality::Balanced => "Balanced",
            Quality::High => "High",
            Quality::Ultra => "Ultra",
        }
    }

    fn bpp(self) -> f32 {
        match self {
            Quality::Low => 0.06,
            Quality::Balanced => 0.12,
            Quality::High => 0.20,
            Quality::Ultra => 0.34,
        }
    }
}

struct MonitorInfo {
    handle: Monitor,
    device_name: String,
    label: String,
}

struct WindowInfo {
    handle: Window,
    label: String,
}

enum Task {
    Idle,
    Countdown { secs_left: u8, deadline: Instant },
    Starting(Receiver<Result<ActiveRecording, String>>),
    Stopping(Receiver<Result<PathBuf, String>>),
    PickingFolder(Receiver<Option<PathBuf>>),
    Screenshot(Receiver<Result<PathBuf, String>>),
}

struct RegionPicker {
    viewport_id: egui::ViewportId,
    monitor: MonitorBounds,
    start: Option<egui::Pos2>,
    current: Option<egui::Pos2>,
    result: Option<Region>,
    cancelled: bool,
    preview_rx: Option<Receiver<Result<recorder::PreviewFrame, String>>>,
    preview_texture: Option<egui::TextureHandle>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
struct Prefs {
    fps: u32,
    quality: u8,
    codec: u8,
    system_audio: bool,
    microphone: bool,
    capture_cursor: bool,
    countdown_secs: u8,
    minimize_on_record: bool,
    hotkeys_enabled: bool,
    output_dir: String,
    target_mode: u8,
    monitor_index: usize,
    region_enabled: bool,
    region_x: u32,
    region_y: u32,
    region_width: u32,
    region_height: u32,
    max_duration_secs: u64,
    webcam_enabled: bool,
    webcam_device: usize,
    webcam_size: u8,
    webcam_corner: u8,
    webcam_mirror: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            fps: 60,
            quality: 1,
            codec: 0,
            system_audio: true,
            microphone: false,
            capture_cursor: true,
            countdown_secs: 3,
            minimize_on_record: true,
            hotkeys_enabled: true,
            output_dir: util::recordings_dir().to_string_lossy().to_string(),
            target_mode: 0,
            monitor_index: 0,
            region_enabled: false,
            region_x: 0,
            region_y: 0,
            region_width: 0,
            region_height: 0,
            max_duration_secs: 0,
            webcam_enabled: false,
            webcam_device: 0,
            webcam_size: 22,
            webcam_corner: 3,
            webcam_mirror: true,
        }
    }
}

pub struct App {
    monitors: Vec<MonitorInfo>,
    windows: Vec<WindowInfo>,
    mode: TargetMode,
    selected_monitor: usize,
    selected_window: usize,
    fps: u32,
    quality: Quality,
    codec: Codec,
    system_audio: bool,
    microphone: bool,
    capture_cursor: bool,
    countdown_secs: u8,
    minimize_on_record: bool,
    hotkeys_enabled: bool,
    output_dir: String,

    region_enabled: bool,
    region_x: u32,
    region_y: u32,
    region_width: u32,
    region_height: u32,
    region_picker: Option<RegionPicker>,

    max_duration_secs: u64,

    webcam_enabled: bool,
    webcam_devices: Vec<String>,
    webcam_device: usize,
    webcam_size: u8,
    webcam_corner: Corner,
    webcam_mirror: bool,

    history: Vec<HistoryEntry>,
    pending_delete: Option<String>,
    last_screenshot: Option<PathBuf>,
    pending_duration: Option<f64>,

    recording: Option<ActiveRecording>,
    task: Task,
    error: Option<String>,
    last_saved: Option<PathBuf>,
    last_saved_at: Option<Instant>,

    fps_mark: Instant,
    fps_frames: u64,
    display_fps: f32,
    started_at: Option<Instant>,

    logo_texture: egui::TextureHandle,
    _hotkey_listener: Option<HotkeyListener>,
    hotkey_rx: Option<Receiver<HotkeyAction>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let prefs = load_prefs();
        let logo_texture = load_logo_texture(&cc.egui_ctx);

        let (hotkey_listener, hotkey_rx) = if prefs.hotkeys_enabled {
            let (listener, rx) = HotkeyListener::new();
            (Some(listener), Some(rx))
        } else {
            (None, None)
        };

        let mut app = Self {
            monitors: Vec::new(),
            windows: Vec::new(),
            mode: if prefs.target_mode == 1 {
                TargetMode::Window
            } else {
                TargetMode::Monitor
            },
            selected_monitor: prefs.monitor_index,
            selected_window: 0,
            fps: prefs.fps.clamp(1, 240),
            quality: match prefs.quality {
                0 => Quality::Low,
                2 => Quality::High,
                3 => Quality::Ultra,
                _ => Quality::Balanced,
            },
            codec: if prefs.codec == 1 {
                Codec::Hevc
            } else {
                Codec::H264
            },
            system_audio: prefs.system_audio,
            microphone: prefs.microphone,
            capture_cursor: prefs.capture_cursor,
            countdown_secs: prefs.countdown_secs,
            minimize_on_record: prefs.minimize_on_record,
            hotkeys_enabled: prefs.hotkeys_enabled,
            output_dir: if prefs.output_dir.is_empty() {
                util::recordings_dir().to_string_lossy().to_string()
            } else {
                prefs.output_dir
            },
            region_enabled: prefs.region_enabled,
            region_x: prefs.region_x,
            region_y: prefs.region_y,
            region_width: prefs.region_width,
            region_height: prefs.region_height,
            region_picker: None,
            max_duration_secs: prefs.max_duration_secs,
            webcam_enabled: prefs.webcam_enabled,
            webcam_devices: Vec::new(),
            webcam_device: prefs.webcam_device,
            webcam_size: prefs.webcam_size.clamp(8, 60),
            webcam_corner: Corner::from_index(prefs.webcam_corner),
            webcam_mirror: prefs.webcam_mirror,
            history: history::load(),
            pending_delete: None,
            last_screenshot: None,
            pending_duration: None,
            recording: None,
            task: Task::Idle,
            error: None,
            last_saved: None,
            last_saved_at: None,
            fps_mark: Instant::now(),
            fps_frames: 0,
            display_fps: 0.0,
            started_at: None,
            logo_texture,
            _hotkey_listener: hotkey_listener,
            hotkey_rx,
        };

        app.refresh_sources();
        if app.selected_monitor >= app.monitors.len() {
            app.selected_monitor = 0;
        }
        app.refresh_webcams();
        app
    }

    fn refresh_sources(&mut self) {
        self.monitors = Monitor::enumerate()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, handle)| {
                let name = handle.name().unwrap_or_default();
                let name = if name.trim().is_empty() {
                    let idx = handle.index().unwrap_or(i + 1);
                    format!("Display {idx}")
                } else {
                    name
                };
                let w = handle.width().unwrap_or(0);
                let h = handle.height().unwrap_or(0);
                let hz = handle.refresh_rate().unwrap_or(60);
                let device_name = handle.device_name().unwrap_or_default();
                MonitorInfo {
                    handle,
                    device_name,
                    label: format!("{name}  ({w}x{h} @ {hz}Hz)"),
                }
            })
            .collect();

        self.windows = Window::enumerate()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|handle| {
                if !handle.is_valid() {
                    return None;
                }
                let w = handle.width().unwrap_or(0);
                let h = handle.height().unwrap_or(0);
                if w <= 10 || h <= 10 {
                    return None;
                }
                let title = handle.title().ok()?;
                let trimmed = title.trim();
                if trimmed.is_empty() || trimmed == "ScreenRec" || trimmed == "Program Manager" {
                    return None;
                }
                let proc = handle.process_name().unwrap_or_default();
                let label = if proc.is_empty() {
                    format!("{} ({w}x{h})", truncate(trimmed, 38))
                } else {
                    format!("{proc} · {} ({w}x{h})", truncate(trimmed, 30))
                };
                Some(WindowInfo { handle, label })
            })
            .collect();

        if self.selected_monitor >= self.monitors.len() {
            self.selected_monitor = 0;
        }
        if self.selected_window >= self.windows.len() {
            self.selected_window = 0;
        }
    }

    fn current_target(&self) -> Option<CaptureTarget> {
        match self.mode {
            TargetMode::Monitor => self
                .monitors
                .get(self.selected_monitor)
                .map(|m| CaptureTarget::Monitor(m.handle)),
            TargetMode::Window => self
                .windows
                .get(self.selected_window)
                .map(|w| CaptureTarget::Window(w.handle)),
        }
    }

    fn build_options(&self) -> Result<RecordingOptions, String> {
        let target = self
            .current_target()
            .ok_or_else(|| "Select a display or window to record.".to_string())?;

        let region = if self.mode == TargetMode::Monitor && self.region_enabled {
            let region = Region {
                x: self.region_x,
                y: self.region_y,
                width: self.region_width,
                height: self.region_height,
            };
            Some(
                region
                    .normalized(target.width(), target.height())
                    .ok_or_else(|| {
                        "The selected region is invalid. Pick a region or use the full display."
                            .to_string()
                    })?,
            )
        } else {
            None
        };

        let (out_width, out_height) = match region {
            Some(r) => (r.width, r.height),
            None => (target.width(), target.height()),
        };

        let width = out_width.max(2) as f32;
        let height = out_height.max(2) as f32;
        let raw = width * height * self.fps as f32 * self.quality.bpp();
        let bitrate = (raw as u64).clamp(4_000_000, 120_000_000) as u32;

        let webcam = if self.webcam_enabled && !self.webcam_devices.is_empty() {
            Some(WebcamOptions {
                device_index: self.webcam_device.min(self.webcam_devices.len() - 1),
                size_pct: self.webcam_size,
                corner: self.webcam_corner,
                mirror: self.webcam_mirror,
            })
        } else {
            None
        };

        let dir = PathBuf::from(self.output_dir.trim());
        let dir = if dir.as_os_str().is_empty() {
            util::recordings_dir()
        } else {
            dir
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create output folder {}: {e}", dir.display()))?;

        let output = util::timestamped_path(&dir, "mp4");

        Ok(RecordingOptions {
            target,
            output,
            fps: self.fps,
            bitrate,
            codec: self.codec,
            system_audio: self.system_audio,
            microphone: self.microphone,
            capture_cursor: self.capture_cursor,
            region,
            webcam,
        })
    }

    fn request_start(&mut self) {
        self.error = None;
        self.last_saved = None;
        self.last_saved_at = None;
        if self.countdown_secs > 0 {
            self.task = Task::Countdown {
                secs_left: self.countdown_secs,
                deadline: Instant::now() + Duration::from_secs(1),
            };
        } else {
            self.begin_start();
        }
    }

    fn begin_start(&mut self) {
        self.error = None;

        let options = match self.build_options() {
            Ok(options) => options,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };

        self.save_prefs();

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("screenrec-start".to_string())
            .spawn(move || {
                let result = recorder::start(options).map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            })
            .ok();

        self.task = Task::Starting(rx);
    }

    fn begin_stop(&mut self) {
        if let Some(recording) = self.recording.take() {
            self.pending_duration = Some(recording.elapsed().as_secs_f64());
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("screenrec-stop".to_string())
                .spawn(move || {
                    let result = recording.stop().map_err(|e| format!("{e:#}"));
                    let _ = tx.send(result);
                })
                .ok();
            self.task = Task::Stopping(rx);
        }
    }

    fn take_screenshot(&mut self) {
        if matches!(
            self.task,
            Task::Starting(_) | Task::Stopping(_) | Task::PickingFolder(_) | Task::Screenshot(_)
        ) {
            return;
        }
        let Some(target) = self.current_target() else {
            self.error = Some("Select a display or window first.".to_string());
            return;
        };

        let region = if self.mode == TargetMode::Monitor && self.region_enabled {
            Region {
                x: self.region_x,
                y: self.region_y,
                width: self.region_width,
                height: self.region_height,
            }
            .normalized(target.width(), target.height())
        } else {
            None
        };

        let dir = util::screenshots_dir();
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("Cannot create screenshot folder: {err}"));
            return;
        }
        let path = util::timestamped_path(&dir, "png");

        self.error = None;
        self.last_saved = None;
        self.last_saved_at = None;

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("screenrec-screenshot".to_string())
            .spawn(move || {
                let result =
                    recorder::screenshot(target, region, path).map_err(|err| format!("{err:#}"));
                let _ = tx.send(result);
            })
            .ok();

        self.task = Task::Screenshot(rx);
    }

    fn refresh_webcams(&mut self) {
        self.webcam_devices = camera::enumerate_devices();
        if self.webcam_device >= self.webcam_devices.len() {
            self.webcam_device = 0;
        }
    }

    fn start_region_picker(&mut self) {
        let Some(monitor) = self.monitors.get(self.selected_monitor) else {
            self.error = Some("Select a display first.".to_string());
            return;
        };
        let Some(bounds) = monitor_util::find(&monitor.device_name) else {
            self.error = Some("Could not query the selected display.".to_string());
            return;
        };
        self.error = None;

        let (tx, rx) = mpsc::channel();
        if let Some(target) = self.current_target() {
            std::thread::spawn(move || {
                let result = recorder::grab(target, None).map_err(|err| err.to_string());
                let _ = tx.send(result);
            });
        }

        self.region_picker = Some(RegionPicker {
            viewport_id: egui::ViewportId::from_hash_of("screenrec-region-picker"),
            monitor: bounds,
            start: None,
            current: None,
            result: None,
            cancelled: false,
            preview_rx: Some(rx),
            preview_texture: None,
        });
    }

    fn drive_region_picker(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.region_picker.take() else {
            return;
        };

        if picker.preview_texture.is_none() {
            let received = picker.preview_rx.as_ref().map(|rx| rx.try_recv());
            match received {
                Some(Ok(Ok(frame))) => {
                    let mut rgba = frame.bgra;
                    for pixel in rgba.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &rgba,
                    );
                    picker.preview_texture = Some(ctx.load_texture(
                        "region-picker-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                    picker.preview_rx = None;
                }
                Some(Ok(Err(_))) | Some(Err(TryRecvError::Disconnected)) => {
                    picker.preview_rx = None;
                }
                _ => {}
            }
        }

        let (px, py) = picker.monitor.logical_position();
        let (sw, sh) = picker.monitor.logical_size();
        let builder = egui::ViewportBuilder::default()
            .with_title("Select recording region")
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_transparent(true)
            .with_icon(window_icon())
            .with_position(egui::pos2(px, py))
            .with_inner_size([sw, sh]);

        let mut finished = false;

        ctx.show_viewport_immediate(picker.viewport_id, builder, |ui, _class| {
            let ctx = ui.ctx().clone();
            let screen = ctx.content_rect();

            if ctx.input(|i| i.viewport().close_requested()) {
                picker.cancelled = true;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                || ctx.input(|i| i.pointer.secondary_clicked())
            {
                picker.cancelled = true;
            }

            let pointer = ctx.input(|i| i.pointer.interact_pos());
            let primary_down = ctx.input(|i| i.pointer.primary_down());
            let primary_released = ctx.input(|i| i.pointer.primary_released());

            if primary_down {
                if picker.start.is_none() {
                    picker.start = pointer;
                }
                if let Some(pos) = pointer {
                    picker.current = Some(pos);
                }
            }

            if primary_released && picker.start.is_some() {
                if let (Some(a), Some(b)) = (picker.start, picker.current) {
                    let rect = egui::Rect::from_two_pos(a, b);
                    if rect.width() > 8.0 && rect.height() > 8.0 && screen.width() > 0.0 {
                        let nx = (rect.min.x - screen.min.x) / screen.width();
                        let ny = (rect.min.y - screen.min.y) / screen.height();
                        let nx2 = (rect.max.x - screen.min.x) / screen.width();
                        let ny2 = (rect.max.y - screen.min.y) / screen.height();
                        let region = Region {
                            x: (nx * picker.monitor.width as f32)
                                .round()
                                .clamp(0.0, picker.monitor.width as f32)
                                as u32,
                            y: (ny * picker.monitor.height as f32)
                                .round()
                                .clamp(0.0, picker.monitor.height as f32)
                                as u32,
                            width: ((nx2 - nx) * picker.monitor.width as f32).round().max(2.0)
                                as u32,
                            height: ((ny2 - ny) * picker.monitor.height as f32).round().max(2.0)
                                as u32,
                        };
                        picker.result = Some(
                            region
                                .normalized(picker.monitor.width, picker.monitor.height)
                                .unwrap_or(region),
                        );
                    }
                }
                finished = true;
            }

            if picker.cancelled {
                finished = true;
            }

            let selection = match (picker.start, picker.current) {
                (Some(a), Some(b)) => {
                    let r = egui::Rect::from_two_pos(a, b);
                    Some(r.intersect(screen))
                }
                _ => None,
            };

            let painter = ui.painter();
            if let Some(texture) = &picker.preview_texture {
                painter.image(
                    texture.id(),
                    screen,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            let dim = Color32::from_black_alpha(120);

            if let Some(sel) = selection {
                if sel.width() > 0.0 && sel.height() > 0.0 {
                    painter.rect_filled(
                        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, sel.min.y)),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(screen.min.x, sel.max.y), screen.max),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(screen.min.x, sel.min.y),
                            egui::pos2(sel.min.x, sel.max.y),
                        ),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(sel.max.x, sel.min.y),
                            egui::pos2(screen.max.x, sel.max.y),
                        ),
                        0.0,
                        dim,
                    );
                    painter.rect_stroke(
                        sel,
                        CornerRadius::same(2),
                        Stroke::new(2.0, ACCENT_HI),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        egui::pos2(sel.min.x + 6.0, sel.min.y + 6.0),
                        egui::Align2::LEFT_TOP,
                        format!(
                            "{} x {}",
                            sel.width().round() as i32,
                            sel.height().round() as i32
                        ),
                        egui::FontId::monospace(13.0),
                        Color32::WHITE,
                    );
                }
            } else {
                painter.rect_filled(screen, 0.0, dim);
            }

            painter.text(
                egui::pos2(screen.center().x, screen.min.y + 28.0),
                egui::Align2::CENTER_TOP,
                if picker.preview_texture.is_some() {
                    "Drag to select the recording area  ·  Esc to cancel"
                } else {
                    "Preparing preview…  ·  Drag to select  ·  Esc to cancel"
                },
                egui::FontId::proportional(16.0),
                Color32::WHITE,
            );

            if finished {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint();
            }
        });

        if finished {
            if let Some(region) = picker.result {
                self.region_enabled = true;
                self.region_x = region.x;
                self.region_y = region.y;
                self.region_width = region.width;
                self.region_height = region.height;
                self.save_prefs();
            }
        } else {
            self.region_picker = Some(picker);
        }
    }

    fn poll_tasks(&mut self, ctx: &egui::Context) {
        if self.hotkeys_enabled && self.hotkey_rx.is_none() {
            let (listener, rx) = HotkeyListener::new();
            self._hotkey_listener = Some(listener);
            self.hotkey_rx = Some(rx);
        } else if !self.hotkeys_enabled && self.hotkey_rx.is_some() {
            self.hotkey_rx = None;
            self._hotkey_listener = None;
        }

        let hotkey_actions: Vec<HotkeyAction> = if self.hotkeys_enabled {
            if let Some(rx) = &self.hotkey_rx {
                let mut list = Vec::new();
                while let Ok(action) = rx.try_recv() {
                    list.push(action);
                }
                list
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        for action in hotkey_actions {
            match action {
                HotkeyAction::ToggleRecord => {
                    if self.recording.is_some() {
                        self.begin_stop();
                    } else if matches!(self.task, Task::Countdown { .. }) {
                        self.task = Task::Idle;
                    } else if matches!(self.task, Task::Idle) {
                        self.request_start();
                    }
                }
                HotkeyAction::TogglePause => {
                    if let Some(recording) = &self.recording {
                        if recording.is_paused() {
                            recording.resume();
                        } else {
                            recording.pause();
                        }
                    }
                }
                HotkeyAction::Screenshot => {
                    self.take_screenshot();
                }
            }
        }

        if self.max_duration_secs > 0 && !matches!(self.task, Task::Stopping(_)) {
            let reached = self
                .recording
                .as_ref()
                .is_some_and(|r| r.elapsed() >= Duration::from_secs(self.max_duration_secs));
            if reached {
                self.begin_stop();
            }
        }

        let current = std::mem::replace(&mut self.task, Task::Idle);

        match current {
            Task::Countdown {
                mut secs_left,
                deadline,
            } => {
                if Instant::now() >= deadline {
                    secs_left = secs_left.saturating_sub(1);
                    if secs_left == 0 {
                        self.task = Task::Idle;
                        self.begin_start();
                    } else {
                        self.task = Task::Countdown {
                            secs_left,
                            deadline: Instant::now() + Duration::from_secs(1),
                        };
                    }
                } else {
                    self.task = Task::Countdown {
                        secs_left,
                        deadline,
                    };
                }
            }
            Task::Starting(rx) => match rx.try_recv() {
                Ok(Ok(recording)) => {
                    self.recording = Some(recording);
                    self.started_at = Some(Instant::now());
                    self.fps_mark = Instant::now();
                    self.fps_frames = 0;
                    self.display_fps = 0.0;

                    if self.minimize_on_record {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                }
                Ok(Err(err)) => self.error = Some(err),
                Err(TryRecvError::Empty) => self.task = Task::Starting(rx),
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("Recording failed to start.".to_string());
                }
            },
            Task::Stopping(rx) => match rx.try_recv() {
                Ok(Ok(path)) => {
                    let duration = self.pending_duration.take().unwrap_or(0.0);
                    self.history = history::add(
                        self.history.clone(),
                        HistoryEntry::new(&path, "video", duration),
                    );
                    self.last_saved = Some(path);
                    self.last_saved_at = Some(Instant::now());
                    self.started_at = None;

                    if self.minimize_on_record {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
                Ok(Err(err)) => {
                    self.pending_duration = None;
                    self.error = Some(err);
                    if self.minimize_on_record {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    }
                }
                Err(TryRecvError::Empty) => self.task = Task::Stopping(rx),
                Err(TryRecvError::Disconnected) => {
                    self.pending_duration = None;
                    self.error = Some("Failed to finalize recording.".to_string());
                    if self.minimize_on_record {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    }
                }
            },
            Task::PickingFolder(rx) => match rx.try_recv() {
                Ok(Some(path)) => {
                    self.output_dir = path.to_string_lossy().to_string();
                    self.save_prefs();
                }
                Ok(None) => {}
                Err(TryRecvError::Empty) => self.task = Task::PickingFolder(rx),
                Err(TryRecvError::Disconnected) => {}
            },
            Task::Screenshot(rx) => match rx.try_recv() {
                Ok(Ok(path)) => {
                    self.last_screenshot = Some(path.clone());
                    self.last_saved = Some(path.clone());
                    self.last_saved_at = Some(Instant::now());
                    self.history = history::add(
                        self.history.clone(),
                        HistoryEntry::new(&path, "screenshot", 0.0),
                    );
                }
                Ok(Err(err)) => self.error = Some(format!("Screenshot failed: {err}")),
                Err(TryRecvError::Empty) => self.task = Task::Screenshot(rx),
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("Screenshot failed.".to_string());
                }
            },
            Task::Idle => {}
        }
    }

    fn browse_output_folder(&mut self) {
        let current = self.output_dir.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("screenrec-browse".to_string())
            .spawn(move || {
                let script = format!(
                    "Add-Type -AssemblyName System.Windows.Forms; $f = New-Object System.Windows.Forms.FolderBrowserDialog; $f.SelectedPath = '{}'; if ($f.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ Write-Output $f.SelectedPath }}",
                    current.replace('\'', "''")
                );
                let output = std::process::Command::new("powershell")
                    .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
                    .output()
                    .ok();
                let path = output.and_then(|out| {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !s.is_empty() && std::path::Path::new(&s).is_dir() {
                        Some(PathBuf::from(s))
                    } else {
                        None
                    }
                });
                let _ = tx.send(path);
            })
            .ok();
        self.task = Task::PickingFolder(rx);
    }

    fn save_prefs(&self) {
        let prefs = Prefs {
            fps: self.fps,
            quality: match self.quality {
                Quality::Low => 0,
                Quality::Balanced => 1,
                Quality::High => 2,
                Quality::Ultra => 3,
            },
            codec: if self.codec == Codec::Hevc { 1 } else { 0 },
            system_audio: self.system_audio,
            microphone: self.microphone,
            capture_cursor: self.capture_cursor,
            countdown_secs: self.countdown_secs,
            minimize_on_record: self.minimize_on_record,
            hotkeys_enabled: self.hotkeys_enabled,
            output_dir: self.output_dir.clone(),
            target_mode: if self.mode == TargetMode::Window {
                1
            } else {
                0
            },
            monitor_index: self.selected_monitor,
            region_enabled: self.region_enabled,
            region_x: self.region_x,
            region_y: self.region_y,
            region_width: self.region_width,
            region_height: self.region_height,
            max_duration_secs: self.max_duration_secs,
            webcam_enabled: self.webcam_enabled,
            webcam_device: self.webcam_device,
            webcam_size: self.webcam_size,
            webcam_corner: self.webcam_corner.index(),
            webcam_mirror: self.webcam_mirror,
        };

        if let Ok(text) = serde_json::to_string_pretty(&prefs) {
            if let Some(dir) = util::config_dir() {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join("config.json"), text);
            }
        }
    }

    fn update_stats(&mut self) {
        let (frames, _) = match &self.recording {
            Some(recording) => (recording.frames(), recording.elapsed()),
            None => return,
        };

        let now = Instant::now();
        let dt = now.duration_since(self.fps_mark).as_secs_f32();
        if dt >= 0.35 {
            let delta = frames.saturating_sub(self.fps_frames) as f32;
            let measured = delta / dt;
            self.display_fps = if self.display_fps <= 0.1 {
                measured
            } else {
                self.display_fps * 0.55 + measured * 0.45
            };
            self.fps_frames = frames;
            self.fps_mark = now;
        }
    }

    fn output_dimensions(&self) -> (u32, u32) {
        if self.mode == TargetMode::Monitor
            && self.region_enabled
            && self.region_width > 1
            && self.region_height > 1
        {
            return (self.region_width, self.region_height);
        }

        let (w, h) = match self.mode {
            TargetMode::Monitor => self.monitors.get(self.selected_monitor).map(|m| {
                (
                    m.handle.width().unwrap_or(0),
                    m.handle.height().unwrap_or(0),
                )
            }),
            TargetMode::Window => self.windows.get(self.selected_window).map(|w| {
                (
                    w.handle.width().unwrap_or(0).max(0) as u32,
                    w.handle.height().unwrap_or(0).max(0) as u32,
                )
            }),
        }
        .unwrap_or((0, 0));

        if w == 0 || h == 0 {
            (1920, 1080)
        } else {
            (w, h)
        }
    }

    fn source_summary(&self) -> String {
        match self.mode {
            TargetMode::Monitor => self
                .monitors
                .get(self.selected_monitor)
                .map(|m| m.label.clone())
                .unwrap_or_else(|| "No display selected".to_string()),
            TargetMode::Window => self
                .windows
                .get(self.selected_window)
                .map(|w| w.label.clone())
                .unwrap_or_else(|| "No window selected".to_string()),
        }
    }

    fn header(&self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new()
            .fill(PANEL_TOP)
            .inner_margin(Margin::symmetric(20, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(&self.logo_texture)
                            .fit_to_exact_size(Vec2::splat(34.0))
                            .corner_radius(CornerRadius::same(9)),
                    );
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("ScreenRec").size(18.0).strong().color(TEXT));
                        ui.label(
                            RichText::new("Screen recording, refined")
                                .size(10.5)
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        pill(ui, "v0.1.0", TEXT_SOFT, SURFACE_ELEVATED, BORDER);
                        ui.add_space(12.0);
                        shortcut(ui, "F10", "Pause");
                        ui.add_space(10.0);
                        shortcut(ui, "F9", "Record");
                    });
                });
            });

        let rect = frame.response.rect;
        ui.painter().line_segment(
            [
                pos2(rect.left(), rect.bottom()),
                pos2(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, BORDER_SOFT),
        );
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new()
            .fill(PANEL_TOP)
            .inner_margin(Margin::symmetric(20, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (label, color) = if self.error.is_some() {
                        ("Attention needed", DANGER)
                    } else if self.recording.is_some() {
                        ("Recording", DANGER)
                    } else if !matches!(self.task, Task::Idle) {
                        ("Working", CAUTION)
                    } else {
                        ("Ready", SUCCESS)
                    };

                    status_dot(ui, color);
                    ui.add_space(5.0);
                    ui.label(RichText::new(label).size(10.5).color(MUTED));

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if self.hotkeys_enabled {
                            ui.label(
                                RichText::new("F9 record  ·  F10 pause  ·  F11 screenshot")
                                    .size(9.5)
                                    .color(MUTED_DIM),
                            );
                        } else {
                            ui.label(
                                RichText::new("Global hotkeys disabled")
                                    .size(9.5)
                                    .color(MUTED_DIM),
                            );
                        }
                    });
                });
            });

        let rect = frame.response.rect;
        ui.painter().line_segment(
            [
                pos2(rect.left(), rect.top()),
                pos2(rect.right(), rect.top()),
            ],
            Stroke::new(1.0, BORDER_SOFT),
        );
    }

    fn source_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Monitor, "Capture", "Source and area");
            ui.add_space(10.0);

            segmented(
                ui,
                "target_mode",
                &mut self.mode,
                &[
                    (TargetMode::Monitor, Icon::Monitor, "Display"),
                    (TargetMode::Window, Icon::Window, "Window"),
                ],
            );
            ui.add_space(10.0);

            match self.mode {
                TargetMode::Monitor => {
                    field_label(ui, Some(Icon::Monitor), "Display");
                    let labels: Vec<String> =
                        self.monitors.iter().map(|m| m.label.clone()).collect();
                    let current = labels
                        .get(self.selected_monitor)
                        .cloned()
                        .unwrap_or_else(|| "No displays detected".to_string());
                    select_index(ui, "monitor", &current, &labels, &mut self.selected_monitor);

                    ui.add_space(8.0);
                    toggle_row(
                        ui,
                        Icon::Crop,
                        "Record a region",
                        "Limit capture to part of the display",
                        &mut self.region_enabled,
                    );

                    if self.region_enabled {
                        ui.add_space(8.0);
                        let (max_w, max_h) = self
                            .monitors
                            .get(self.selected_monitor)
                            .map(|m| {
                                (
                                    m.handle.width().unwrap_or(0),
                                    m.handle.height().unwrap_or(0),
                                )
                            })
                            .unwrap_or((0, 0));

                        mask(ui, |ui| {
                            ui.horizontal(|ui| {
                                number_field(ui, "X", &mut self.region_x, 0..=max_w);
                                ui.add_space(6.0);
                                number_field(ui, "Y", &mut self.region_y, 0..=max_h);
                                ui.add_space(6.0);
                                number_field(ui, "W", &mut self.region_width, 2..=max_w.max(2));
                                ui.add_space(6.0);
                                number_field(ui, "H", &mut self.region_height, 2..=max_h.max(2));
                            });

                            ui.add_space(8.0);
                            if wide_button(
                                ui,
                                Some(Icon::Crop),
                                "Select region on screen",
                                ButtonKind::Secondary,
                                34.0,
                            )
                            .clicked()
                            {
                                self.start_region_picker();
                            }

                            if self.region_width <= 1 || self.region_height <= 1 {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Drag over the display to set an area.")
                                        .size(10.5)
                                        .color(CAUTION),
                                );
                            }
                        });
                    }
                }
                TargetMode::Window => {
                    field_label(ui, Some(Icon::Window), "Window");
                    let labels: Vec<String> =
                        self.windows.iter().map(|w| w.label.clone()).collect();
                    let current = labels
                        .get(self.selected_window)
                        .cloned()
                        .unwrap_or_else(|| "No windows detected".to_string());
                    select_index(ui, "window", &current, &labels, &mut self.selected_window);

                    if self.windows.is_empty() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("No visible windows found. Open an app, then refresh.")
                                .size(10.5)
                                .color(CAUTION),
                        );
                        ui.add_space(6.0);
                        if wide_button(
                            ui,
                            Some(Icon::Refresh),
                            "Refresh window list",
                            ButtonKind::Secondary,
                            34.0,
                        )
                        .clicked()
                        {
                            self.refresh_sources();
                        }
                    }
                }
            }
        });
    }

    fn video_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Sliders, "Video", "Frame rate, quality and codec");
            ui.add_space(10.0);

            field_label(ui, Some(Icon::Timer), "Frame rate");
            segmented(
                ui,
                "fps",
                &mut self.fps,
                &[
                    (24u32, Icon::Timer, "24"),
                    (30, Icon::Timer, "30"),
                    (60, Icon::Timer, "60"),
                    (120, Icon::Timer, "120"),
                ],
            );
            ui.add_space(10.0);

            ui.columns(2, |columns| {
                columns[0].vertical(|ui| {
                    field_label(ui, Some(Icon::Film), "Quality");
                    select_enum(
                        ui,
                        "quality",
                        self.quality.label(),
                        &Quality::ALL.map(|quality| (quality, quality.label())),
                        &mut self.quality,
                    );
                });

                columns[1].vertical(|ui| {
                    field_label(ui, Some(Icon::Chip), "Codec");
                    select_enum(
                        ui,
                        "codec",
                        self.codec.label(),
                        &[(Codec::H264, "H.264"), (Codec::Hevc, "HEVC")],
                        &mut self.codec,
                    );
                });
            });

            ui.add_space(10.0);
            let (w, h) = self.output_dimensions();
            mask(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Output").size(10.5).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{w} × {h}"))
                                .size(11.0)
                                .strong()
                                .color(TEXT_SOFT),
                        );
                    });
                });
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!(
                        "{} fps · {} · hardware accelerated",
                        self.fps,
                        self.codec.label()
                    ))
                    .size(10.0)
                    .color(MUTED_DIM),
                );
            });
        });
    }

    fn audio_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Speaker, "Audio & input", "What gets captured");
            ui.add_space(8.0);

            toggle_row(
                ui,
                Icon::Speaker,
                "System audio",
                "Capture speakers or headphones",
                &mut self.system_audio,
            );
            toggle_row(
                ui,
                Icon::Microphone,
                "Microphone",
                "Add voice narration",
                &mut self.microphone,
            );
            toggle_row(
                ui,
                Icon::Cursor,
                "Mouse cursor",
                "Show the pointer in the recording",
                &mut self.capture_cursor,
            );
        });
    }

    fn webcam_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Camera, "Camera", "Webcam overlay");
            ui.add_space(8.0);

            toggle_row(
                ui,
                Icon::Camera,
                "Show webcam bubble",
                "Overlay your camera on the capture",
                &mut self.webcam_enabled,
            );

            if self.webcam_enabled {
                ui.add_space(8.0);
                mask(ui, |ui| {
                    if self.webcam_devices.is_empty() {
                        ui.label(
                            RichText::new("No camera detected. Connect a camera and refresh.")
                                .size(10.5)
                                .color(CAUTION),
                        );
                        ui.add_space(6.0);
                        if wide_button(
                            ui,
                            Some(Icon::Refresh),
                            "Refresh cameras",
                            ButtonKind::Secondary,
                            32.0,
                        )
                        .clicked()
                        {
                            self.refresh_webcams();
                        }
                    } else {
                        field_label(ui, Some(Icon::Camera), "Camera");
                        let labels: Vec<String> = self
                            .webcam_devices
                            .iter()
                            .map(|name| truncate(name, 48))
                            .collect();
                        let current = labels
                            .get(self.webcam_device)
                            .cloned()
                            .unwrap_or_else(|| "Camera".to_string());
                        select_index(
                            ui,
                            "webcam_device",
                            &current,
                            &labels,
                            &mut self.webcam_device,
                        );

                        ui.add_space(8.0);
                        field_label(ui, Some(Icon::Sliders), "Size");
                        ui.add(
                            egui::Slider::new(&mut self.webcam_size, 8..=60)
                                .suffix("%")
                                .show_value(true),
                        );

                        ui.add_space(8.0);
                        field_label(ui, Some(Icon::Crop), "Position");
                        select_enum(
                            ui,
                            "webcam_corner",
                            self.webcam_corner.label(),
                            &Corner::ALL.map(|corner| (corner, corner.label())),
                            &mut self.webcam_corner,
                        );

                        ui.add_space(4.0);
                        toggle_row(
                            ui,
                            Icon::Cursor,
                            "Mirror camera",
                            "Flip the preview horizontally",
                            &mut self.webcam_mirror,
                        );
                    }
                });
            }
        });
    }

    fn preferences_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Record, "Recording", "Timing and shortcuts");
            ui.add_space(10.0);

            field_label(ui, Some(Icon::Timer), "Countdown");
            segmented(
                ui,
                "countdown",
                &mut self.countdown_secs,
                &[
                    (0u8, Icon::Timer, "Off"),
                    (3, Icon::Timer, "3 sec"),
                    (5, Icon::Timer, "5 sec"),
                    (10, Icon::Timer, "10 sec"),
                ],
            );

            ui.add_space(10.0);
            field_label(ui, Some(Icon::Stop), "Auto-stop");
            segmented(
                ui,
                "max_duration",
                &mut self.max_duration_secs,
                &[
                    (0u64, Icon::Stop, "Off"),
                    (60, Icon::Stop, "1m"),
                    (300, Icon::Stop, "5m"),
                    (600, Icon::Stop, "10m"),
                    (1800, Icon::Stop, "30m"),
                    (3600, Icon::Stop, "60m"),
                ],
            );

            ui.add_space(8.0);
            toggle_row(
                ui,
                Icon::Window,
                "Minimize while recording",
                "Keep the capture clean and unobstructed",
                &mut self.minimize_on_record,
            );
            toggle_row(
                ui,
                Icon::Keyboard,
                "Global hotkeys",
                "F9 record · F10 pause · F11 screenshot",
                &mut self.hotkeys_enabled,
            );
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let countdown = match &self.task {
            Task::Countdown { secs_left, .. } => Some(*secs_left),
            _ => None,
        };

        card(ui, |ui| {
            if let Some(recording) = &self.recording {
                let elapsed = recording.elapsed();
                let size = util::human_size(recording.file_size());
                let paused = recording.is_paused();
                let level = (recording.audio_level() as f32 / 100.0).clamp(0.0, 1.0);
                let warning = recording.error();
                let max_duration = self.max_duration_secs;

                ui.horizontal(|ui| {
                    if paused {
                        status_pill(ui, "PAUSED", CAUTION, CAUTION_SOFT);
                    } else {
                        status_pill(ui, "RECORDING", DANGER, DANGER_SOFT);
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        keycap(ui, "F9");
                    });
                });

                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(fmt_duration(elapsed))
                            .font(FontId::monospace(38.0))
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new(if paused {
                            "Recording paused"
                        } else {
                            "Recording in progress"
                        })
                        .size(11.0)
                        .color(MUTED),
                    );
                });
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    stat(ui, "FRAME RATE", &format!("{:.0} fps", self.display_fps));
                    ui.add_space(18.0);
                    stat(ui, "FILE SIZE", &size);
                    if max_duration > 0 {
                        ui.add_space(18.0);
                        let left = max_duration.saturating_sub(elapsed.as_secs());
                        stat(ui, "TIME LEFT", &fmt_duration(Duration::from_secs(left)));
                    }
                });
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Audio level").size(10.5).color(MUTED));
                    let meter_w = (ui.available_width() - 56.0).max(80.0);
                    level_meter(ui, level, meter_w);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{:.0}%", level * 100.0))
                            .size(10.0)
                            .color(MUTED_DIM),
                    );
                });

                if let Some(message) = warning {
                    ui.add_space(8.0);
                    ui.label(RichText::new(message).size(10.5).color(CAUTION));
                }

                ui.add_space(14.0);
                let mut pause_clicked = false;
                let mut stop_clicked = false;
                let mut screenshot_clicked = false;

                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    let pause_kind = if paused {
                        ButtonKind::Success
                    } else {
                        ButtonKind::Secondary
                    };
                    if button_sized(
                        ui,
                        Some(if paused { Icon::Play } else { Icon::Pause }),
                        if paused { "Resume" } else { "Pause" },
                        pause_kind,
                        half,
                        40.0,
                    )
                    .clicked()
                    {
                        pause_clicked = true;
                    }
                    if button_sized(ui, Some(Icon::Stop), "Stop", ButtonKind::Danger, half, 40.0)
                        .clicked()
                    {
                        stop_clicked = true;
                    }
                });

                ui.add_space(8.0);
                if wide_button(
                    ui,
                    Some(Icon::Screenshot),
                    "Take screenshot",
                    ButtonKind::Secondary,
                    36.0,
                )
                .clicked()
                {
                    screenshot_clicked = true;
                }

                if pause_clicked {
                    if paused {
                        recording.resume();
                    } else {
                        recording.pause();
                    }
                }

                if stop_clicked {
                    self.begin_stop();
                } else if screenshot_clicked {
                    self.take_screenshot();
                }
            } else if let Some(secs_left) = countdown {
                ui.horizontal(|ui| {
                    status_pill(ui, "STARTING", CAUTION, CAUTION_SOFT);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        keycap(ui, "F9");
                    });
                });

                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(secs_left.to_string())
                            .font(FontId::monospace(52.0))
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new("Recording will begin shortly")
                            .size(11.5)
                            .color(MUTED),
                    );
                });
                ui.add_space(18.0);

                if wide_button(ui, Some(Icon::Close), "Cancel", ButtonKind::Secondary, 38.0)
                    .clicked()
                {
                    self.task = Task::Idle;
                }
            } else if !matches!(self.task, Task::Idle) {
                let (label, detail) = match &self.task {
                    Task::Starting(_) => (
                        "Initializing encoder",
                        "Preparing capture and audio devices",
                    ),
                    Task::Stopping(_) => ("Finalizing recording", "Writing the video file to disk"),
                    Task::PickingFolder(_) => (
                        "Choosing folder",
                        "Select a destination for your recordings",
                    ),
                    Task::Screenshot(_) => ("Capturing screenshot", "Saving the current frame"),
                    _ => ("Working", "Please wait"),
                };

                ui.horizontal(|ui| {
                    status_pill(ui, "BUSY", ACCENT, ACCENT_SOFT);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        keycap(ui, "F9");
                    });
                });
                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.add(egui::Spinner::new().size(28.0).color(ACCENT_HI));
                    ui.add_space(10.0);
                    ui.label(RichText::new(label).size(13.0).strong().color(TEXT));
                    ui.label(RichText::new(detail).size(10.5).color(MUTED));
                });
                ui.add_space(18.0);
            } else {
                ui.horizontal(|ui| {
                    status_pill(ui, "READY", SUCCESS, SUCCESS_SOFT);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        keycap(ui, "F9");
                    });
                });

                ui.add_space(16.0);
                let mut start_clicked = false;
                let mut screenshot_clicked = false;

                ui.vertical_centered(|ui| {
                    if record_button(ui, 82.0).clicked() {
                        start_clicked = true;
                    }
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Start recording")
                            .size(13.5)
                            .strong()
                            .color(TEXT),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Click the button or press F9")
                            .size(10.5)
                            .color(MUTED),
                    );
                });
                ui.add_space(16.0);

                let (w, h) = self.output_dimensions();
                let source = truncate(&self.source_summary(), 28);
                mask(ui, |ui| {
                    detail_row(ui, Icon::Monitor, "Source", &source);
                    detail_row(
                        ui,
                        Icon::Film,
                        "Output",
                        &format!("{w} × {h} · {} fps", self.fps),
                    );
                    detail_row(
                        ui,
                        Icon::Bolt,
                        "Encoder",
                        &format!("{} · hardware accelerated", self.codec.label()),
                    );
                });

                ui.add_space(12.0);
                if wide_button(
                    ui,
                    Some(Icon::Screenshot),
                    "Take screenshot",
                    ButtonKind::Secondary,
                    38.0,
                )
                .clicked()
                {
                    screenshot_clicked = true;
                }

                if start_clicked {
                    self.request_start();
                }
                if screenshot_clicked {
                    self.take_screenshot();
                }
            }
        });
    }

    fn destination_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Folder, "Destination", "Where captures are saved");
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let browse_w = 78.0;
                let open_w = 62.0;
                let input_w = (ui.available_width() - browse_w - open_w - gap * 2.0).max(80.0);

                let input = egui::TextEdit::singleline(&mut self.output_dir)
                    .text_color(TEXT)
                    .frame(
                        egui::Frame::new()
                            .fill(INPUT_BG)
                            .corner_radius(CornerRadius::same(8))
                            .stroke(Stroke::new(1.0, BORDER))
                            .inner_margin(Margin::symmetric(10, 7)),
                    );
                ui.add_sized(vec2(input_w, 32.0), input);

                if button_sized(ui, None, "Browse", ButtonKind::Secondary, browse_w, 32.0).clicked()
                {
                    self.browse_output_folder();
                }
                if button_sized(ui, None, "Open", ButtonKind::Secondary, open_w, 32.0).clicked() {
                    open_path(Path::new(self.output_dir.trim()));
                }
            });
        });
    }

    fn recent_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_header_left(ui, Icon::Film, "Recent", "Latest captures");
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Open, "Open recordings folder").clicked() {
                        open_path(Path::new(self.output_dir.trim()));
                    }
                });
            });
            ui.add_space(10.0);

            let entries: Vec<HistoryEntry> = self.history.iter().take(6).cloned().collect();
            if entries.is_empty() {
                empty_state(
                    ui,
                    Icon::Film,
                    "No captures yet",
                    "Recordings and screenshots will appear here.",
                );
                return;
            }

            let mut request_delete: Option<String> = None;
            let mut confirm_delete: Option<String> = None;
            let mut cancel_delete = false;

            for entry in &entries {
                let path = entry.path_buf();
                let is_pending = self.pending_delete.as_deref() == Some(entry.path.as_str());
                let (fg, bg) = if entry.kind == "screenshot" {
                    (SUCCESS, SUCCESS_SOFT)
                } else {
                    (ACCENT_HI, ACCENT_SOFT)
                };

                egui::Frame::new()
                    .fill(SURFACE_ELEVATED)
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0, BORDER_SOFT))
                    .inner_margin(Margin::symmetric(8, 7))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            icon_badge_sized(
                                ui,
                                if entry.kind == "screenshot" {
                                    Icon::Image
                                } else {
                                    Icon::Film
                                },
                                fg,
                                bg,
                                30.0,
                            );
                            ui.add_space(8.0);

                            let actions_w = if is_pending { 60.0 } else { 92.0 };
                            let text_w = (ui.available_width() - actions_w - 6.0).max(80.0);
                            ui.allocate_ui_with_layout(
                                vec2(text_w, 32.0),
                                egui::Layout::top_down(Align::Min),
                                |ui| {
                                    ui.label(
                                        RichText::new(truncate(&entry.file_name(), 26))
                                            .size(11.5)
                                            .color(TEXT),
                                    );
                                    ui.add_space(1.0);
                                    let detail = if entry.kind == "screenshot" {
                                        util::human_size(entry.size)
                                    } else {
                                        format!(
                                            "{} · {}",
                                            util::human_duration(
                                                entry.duration_secs.round() as u64
                                            ),
                                            util::human_size(entry.size)
                                        )
                                    };
                                    ui.label(
                                        RichText::new(detail).size(10.0).color(MUTED_DIM),
                                    );
                                },
                            );

                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if is_pending {
                                    if icon_button_sized(ui, Icon::Check, "Delete file", 26.0)
                                        .clicked()
                                    {
                                        confirm_delete = Some(entry.path.clone());
                                    }
                                    if icon_button_sized(ui, Icon::Close, "Cancel", 26.0).clicked()
                                    {
                                        cancel_delete = true;
                                    }
                                } else {
                                    if icon_button_sized(ui, Icon::Trash, "Delete", 26.0).clicked()
                                    {
                                        request_delete = Some(entry.path.clone());
                                    }
                                    if icon_button_sized(ui, Icon::Folder, "Show in folder", 26.0)
                                        .clicked()
                                    {
                                        reveal_path(&path);
                                    }
                                    if icon_button_sized(
                                        ui,
                                        if entry.kind == "screenshot" {
                                            Icon::Open
                                        } else {
                                            Icon::Play
                                        },
                                        if entry.kind == "screenshot" {
                                            "Open"
                                        } else {
                                            "Play"
                                        },
                                        26.0,
                                    )
                                    .clicked()
                                    {
                                        open_path(&path);
                                    }
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
            }

            if let Some(path) = request_delete {
                self.pending_delete = Some(path);
            }
            if cancel_delete {
                self.pending_delete = None;
            }
            if let Some(path) = confirm_delete {
                let _ = std::fs::remove_file(&path);
                self.history.retain(|entry| entry.path != path);
                history::save(&self.history);
                self.pending_delete = None;
            }
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let show_saved = self
            .last_saved_at
            .is_some_and(|saved| saved.elapsed() < Duration::from_secs(10));

        if show_saved && self.last_saved.is_some() {
            ui.add_space(12.0);
            self.saved_toast(ui);
        }

        if self.error.is_some() {
            ui.add_space(12.0);
            self.error_banner(ui);
        }
    }

    fn saved_toast(&self, ui: &mut egui::Ui) {
        let Some(path) = self.last_saved.clone() else {
            return;
        };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let is_video = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
        let open_icon = if is_video { Icon::Play } else { Icon::Open };

        egui::Frame::new()
            .fill(SUCCESS_SOFT)
            .corner_radius(CornerRadius::same(10))
            .stroke(Stroke::new(1.0, SUCCESS.gamma_multiply(0.35)))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    icon_badge_sized(
                        ui,
                        Icon::Check,
                        SUCCESS,
                        Color32::from_rgb(24, 52, 33),
                        30.0,
                    );
                    ui.add_space(9.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Saved successfully")
                                .size(12.0)
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                truncate(&file_name(&path), 28),
                                util::human_size(size)
                            ))
                            .size(10.0)
                            .color(MUTED),
                        );
                    });
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if button(ui, Some(open_icon), "Open", ButtonKind::Secondary).clicked() {
                        open_path(&path);
                    }
                    if button(
                        ui,
                        Some(Icon::Folder),
                        "Show in folder",
                        ButtonKind::Secondary,
                    )
                    .clicked()
                    {
                        reveal_path(&path);
                    }
                });
            });
    }

    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(message) = self.error.clone() else {
            return;
        };

        egui::Frame::new()
            .fill(DANGER_SOFT)
            .corner_radius(CornerRadius::same(10))
            .stroke(Stroke::new(1.0, DANGER_BORDER))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    icon_badge_sized(ui, Icon::Alert, DANGER, Color32::from_rgb(58, 27, 31), 30.0);
                    ui.add_space(9.0);

                    let text_w = (ui.available_width() - 38.0).max(100.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_w, 0.0),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            ui.label(
                                RichText::new("Something went wrong")
                                    .size(12.0)
                                    .strong()
                                    .color(TEXT),
                            );
                            ui.label(
                                RichText::new(&message)
                                    .size(10.5)
                                    .color(Color32::from_rgb(240, 160, 164)),
                            );
                        },
                    );

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, "Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_tasks(&ctx);
        self.drive_region_picker(&ctx);
        self.update_stats();

        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_prefs();
        }

        if self.recording.is_some()
            || !matches!(self.task, Task::Idle)
            || self.region_picker.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(60));
        } else if self
            .last_saved_at
            .is_some_and(|saved| saved.elapsed() < Duration::from_secs(10))
        {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::Panel::top("app_header")
            .exact_size(62.0)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(ui, |ui| {
                self.header(ui);
            });

        egui::Panel::bottom("app_status")
            .exact_size(30.0)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(ui, |ui| {
                self.status_bar(ui);
            });

        egui::Panel::right("control_panel")
            .exact_size(360.0)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(16, 16)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("control_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.controls(ui);
                        self.footer(ui);
                        ui.add_space(12.0);
                        self.destination_card(ui);
                        ui.add_space(12.0);
                        self.recent_card(ui);
                    });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.source_card(ui);
                        ui.add_space(12.0);
                        self.video_card(ui);
                        ui.add_space(12.0);
                        self.audio_card(ui);
                        ui.add_space(12.0);
                        self.webcam_card(ui);
                        ui.add_space(12.0);
                        self.preferences_card(ui);
                    });
            });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonKind {
    Secondary,
    Danger,
    Success,
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .stroke(Stroke::new(1.0, BORDER))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn mask<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(INPUT_BG)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section_header_left(ui: &mut egui::Ui, icon: Icon, title: &str, subtitle: &str) {
    icon_badge(ui, icon, ACCENT_HI, ACCENT_SOFT);
    ui.add_space(9.0);
    ui.vertical(|ui| {
        ui.label(RichText::new(title).size(13.5).strong().color(TEXT));
        ui.label(RichText::new(subtitle).size(10.0).color(MUTED));
    });
}

fn section_header(ui: &mut egui::Ui, icon: Icon, title: &str, subtitle: &str) {
    ui.horizontal(|ui| section_header_left(ui, icon, title, subtitle));
}

fn icon_badge(ui: &mut egui::Ui, icon: Icon, fg: Color32, bg: Color32) {
    icon_badge_sized(ui, icon, fg, bg, 28.0);
}

fn icon_badge_sized(ui: &mut egui::Ui, icon: Icon, fg: Color32, bg: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let radius = CornerRadius::same((size * 0.28).round().clamp(0.0, 255.0) as u8);
    ui.painter().rect_filled(rect, radius, bg);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(size * 0.52)),
        icon,
        fg,
    );
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    icon_button_sized(ui, icon, tooltip, 30.0)
}

fn icon_button_sized(ui: &mut egui::Ui, icon: Icon, tooltip: &str, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), SURFACE_HOVER);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let color = if hovered { TEXT } else { MUTED };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(size * 0.46)),
        icon,
        color,
    );
    response.on_hover_text(tooltip)
}

fn keycap(ui: &mut egui::Ui, key: &str) {
    let font = FontId::monospace(9.5);
    let galley = ui
        .painter()
        .layout_no_wrap(key.to_string(), font, TEXT_SOFT);
    let width = galley.size().x + 12.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(5),
        SURFACE_ELEVATED,
        Stroke::new(1.0, BORDER_HI),
        StrokeKind::Middle,
    );
    ui.painter().galley(
        pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        TEXT_SOFT,
    );
}

fn shortcut(ui: &mut egui::Ui, key: &str, label: &str) {
    ui.horizontal(|ui| {
        keycap(ui, key);
        ui.add_space(5.0);
        ui.label(RichText::new(label).size(10.0).color(MUTED));
    });
}

fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32, border: Color32) {
    let font = FontId::proportional(9.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 14.0, 20.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        bg,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
}

fn status_pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    let font = FontId::proportional(9.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 18.0, 20.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        bg,
        Stroke::new(1.0, fg.gamma_multiply(0.35)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 5.0, color.gamma_multiply(0.14));
    ui.painter().circle_filled(rect.center(), 2.8, color);
}

fn field_label(ui: &mut egui::Ui, icon: Option<Icon>, text: &str) {
    ui.horizontal(|ui| {
        if let Some(icon) = icon {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            icons::paint(ui.painter(), rect, icon, MUTED_DIM);
            ui.add_space(5.0);
        }
        ui.label(RichText::new(text).size(10.5).color(MUTED));
    });
}

fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    options: &[(T, Icon, &str)],
) -> bool {
    if options.is_empty() {
        return false;
    }

    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        INPUT_BG,
        Stroke::new(1.0, BORDER_SOFT),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(3.0);
    let seg_w = inner.width() / options.len() as f32;
    let mut changed = false;

    for (i, (option, icon, label)) in options.iter().enumerate() {
        let seg = Rect::from_min_size(
            pos2(inner.left() + seg_w * i as f32, inner.top()),
            vec2(seg_w, inner.height()),
        );
        let response = ui.interact(seg, ui.scope_id().with((id, i)), Sense::click());

        if response.clicked() && *value != *option {
            *value = *option;
            changed = true;
        }

        let selected = *value == *option;
        let hovered = response.hovered();
        if selected {
            ui.painter().rect(
                seg.shrink(1.0),
                CornerRadius::same(6),
                SURFACE_ACTIVE,
                Stroke::new(1.0, BORDER_HI),
                StrokeKind::Inside,
            );
        } else if hovered {
            ui.painter()
                .rect_filled(seg.shrink(1.0), CornerRadius::same(6), SURFACE_HOVER);
        }

        let fg = if selected {
            TEXT
        } else if hovered {
            TEXT_SOFT
        } else {
            MUTED
        };
        let icon_color = if selected { ACCENT_HI } else { MUTED_DIM };
        let font = FontId::proportional(11.5);
        let galley = ui.painter().layout_no_wrap((*label).to_string(), font, fg);
        let icon_w = 13.0;
        let gap = 6.0;
        let total = icon_w + gap + galley.size().x;
        let start_x = seg.center().x - total / 2.0;

        icons::paint(
            ui.painter(),
            Rect::from_center_size(
                pos2(start_x + icon_w / 2.0, seg.center().y),
                Vec2::splat(13.0),
            ),
            *icon,
            icon_color,
        );
        ui.painter().galley(
            pos2(
                start_x + icon_w + gap,
                seg.center().y - galley.size().y / 2.0,
            ),
            galley,
            fg,
        );

        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }

    changed
}

fn toggle_row(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    subtitle: &str,
    value: &mut bool,
) -> bool {
    let height = if subtitle.is_empty() { 46.0 } else { 54.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());

    let clicked = response.clicked();
    if clicked {
        *value = !*value;
    }

    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), SURFACE_HOVER);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let painter = ui.painter();
    icons::paint(
        painter,
        Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), Vec2::splat(18.0)),
        icon,
        if *value { ACCENT_HI } else { MUTED },
    );

    let text_x = rect.left() + 38.0;
    if subtitle.is_empty() {
        painter.text(
            pos2(text_x, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(12.5),
            TEXT,
        );
    } else {
        painter.text(
            pos2(text_x, rect.center().y - 3.0),
            Align2::LEFT_BOTTOM,
            title,
            FontId::proportional(12.5),
            TEXT,
        );
        painter.text(
            pos2(text_x, rect.center().y + 3.0),
            Align2::LEFT_TOP,
            subtitle,
            FontId::proportional(10.5),
            MUTED,
        );
    }

    let switch =
        Rect::from_center_size(pos2(rect.right() - 34.0, rect.center().y), vec2(40.0, 22.0));
    paint_toggle(ui.ctx(), ui.painter(), response.id, switch, *value, hovered);

    clicked
}

fn paint_toggle(
    ctx: &egui::Context,
    painter: &egui::Painter,
    id: egui::Id,
    rect: Rect,
    on: bool,
    hovered: bool,
) {
    let t = ctx.animate_bool_with_time(id, on, 0.14);
    let track = if on {
        if hovered {
            ACCENT_HI
        } else {
            ACCENT
        }
    } else if hovered {
        Color32::from_rgb(63, 71, 88)
    } else {
        Color32::from_rgb(52, 59, 73)
    };

    painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), track);

    let padding = 3.0;
    let radius = rect.height() / 2.0 - padding;
    let left = rect.left() + padding + radius;
    let right = rect.right() - padding - radius;
    let knob_x = left + (right - left) * t;

    painter.circle_filled(
        pos2(knob_x, rect.center().y + 1.0),
        radius,
        Color32::from_black_alpha(45),
    );
    painter.circle_filled(
        pos2(knob_x, rect.center().y),
        radius,
        if on {
            Color32::WHITE
        } else {
            Color32::from_rgb(220, 224, 232)
        },
    );
}

fn select_index(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    labels: &[String],
    selected: &mut usize,
) -> bool {
    let before = *selected;
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(current).size(11.5).color(TEXT))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            if labels.is_empty() {
                ui.label(
                    RichText::new("No options available")
                        .size(10.5)
                        .color(MUTED),
                );
            }
            for (index, label) in labels.iter().enumerate() {
                ui.selectable_value(selected, index, label.as_str());
            }
        });
    *selected != before
}

fn select_enum<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    options: &[(T, &str)],
    value: &mut T,
) -> bool {
    let before = *value;
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(current).size(11.5).color(TEXT))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for (option, label) in options {
                ui.selectable_value(value, *option, *label);
            }
        });
    *value != before
}

fn number_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(9.5).color(MUTED_DIM));
        ui.add_sized(
            vec2(66.0, 28.0),
            egui::DragValue::new(value).range(range).speed(1.0),
        );
    });
}

fn button(ui: &mut egui::Ui, icon: Option<Icon>, label: &str, kind: ButtonKind) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::proportional(12.0), TEXT);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let width = galley.size().x + icon_w + 26.0;
    button_sized(ui, icon, label, kind, width, 32.0)
}

fn button_sized(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    kind: ButtonKind,
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    paint_button(
        ui.painter(),
        rect,
        icon,
        label,
        kind,
        true,
        response.hovered(),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn wide_button(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    kind: ButtonKind,
    height: f32,
) -> egui::Response {
    button_sized(ui, icon, label, kind, ui.available_width(), height)
}

fn paint_button(
    painter: &egui::Painter,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    kind: ButtonKind,
    enabled: bool,
    hovered: bool,
) {
    let (fill, stroke_color, fg) = match (kind, enabled, hovered) {
        (ButtonKind::Danger, true, false) => (DANGER, DANGER, Color32::WHITE),
        (ButtonKind::Danger, true, true) => (DANGER_HI, DANGER_HI, Color32::WHITE),
        (ButtonKind::Success, true, false) => (SUCCESS, SUCCESS, Color32::from_rgb(8, 20, 11)),
        (ButtonKind::Success, true, true) => (
            Color32::from_rgb(82, 210, 100),
            SUCCESS,
            Color32::from_rgb(8, 20, 11),
        ),
        (ButtonKind::Secondary, true, false) => (SURFACE_ELEVATED, BORDER, TEXT),
        (ButtonKind::Secondary, true, true) => (SURFACE_HOVER, BORDER_HI, TEXT),
        (_, false, _) => (SURFACE_ELEVATED, BORDER_SOFT, MUTED_DIM),
    };

    painter.rect(
        rect,
        CornerRadius::same(8),
        fill,
        Stroke::new(1.0, stroke_color),
        StrokeKind::Middle,
    );

    let galley = painter.layout_no_wrap(label.to_string(), FontId::proportional(12.0), fg);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let total = icon_w + galley.size().x;
    let start_x = rect.center().x - total / 2.0;

    if let Some(icon) = icon {
        icons::paint(
            painter,
            Rect::from_center_size(pos2(start_x + 7.5, rect.center().y), Vec2::splat(15.0)),
            icon,
            fg,
        );
    }

    painter.galley(
        pos2(start_x + icon_w, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
}

fn record_button(ui: &mut egui::Ui, diameter: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let center = rect.center();
    let radius = diameter / 2.0;
    let painter = ui.painter();
    painter.circle_filled(center, radius, DANGER.gamma_multiply(0.12));
    painter.circle_stroke(center, radius - 1.0, Stroke::new(1.0, DANGER_BORDER));
    painter.circle_filled(
        center,
        radius * 0.56,
        if hovered { DANGER_HI } else { DANGER },
    );
    painter.circle_stroke(
        center,
        radius * 0.56 + 1.5,
        Stroke::new(1.0, Color32::from_white_alpha(25)),
    );

    response.on_hover_text("Start recording (F9)")
}

fn level_meter(ui: &mut egui::Ui, level: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 7.0), Sense::hover());
    let radius = CornerRadius::same(3);
    ui.painter().rect_filled(rect, radius, INPUT_BG);

    let fill = rect.width() * level.clamp(0.0, 1.0);
    if fill > 0.5 {
        let color = if level > 0.85 {
            DANGER
        } else if level > 0.6 {
            CAUTION
        } else {
            SUCCESS
        };
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, vec2(fill, rect.height())),
            radius,
            color,
        );
    }
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(9.0).color(MUTED_DIM));
        ui.add_space(1.0);
        ui.label(RichText::new(value).size(14.0).strong().color(TEXT));
    });
}

fn detail_row(ui: &mut egui::Ui, icon: Icon, label: &str, value: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
        icons::paint(ui.painter(), rect, icon, MUTED_DIM);
        ui.add_space(4.0);
        ui.label(RichText::new(label).size(10.5).color(MUTED_DIM));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(10.5).color(TEXT_SOFT));
        });
    });
    ui.add_space(4.0);
}

fn empty_state(ui: &mut egui::Ui, icon: Icon, title: &str, detail: &str) {
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(10), SURFACE_ELEVATED);
        icons::paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(18.0)),
            icon,
            MUTED,
        );
        ui.add_space(8.0);
        ui.label(RichText::new(title).size(12.0).strong().color(TEXT_SOFT));
        ui.label(RichText::new(detail).size(10.5).color(MUTED_DIM));
    });
    ui.add_space(8.0);
}

fn open_path(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

fn reveal_path(path: &Path) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT_SOFT;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT_HI;
    visuals.warn_fg_color = CAUTION;
    visuals.error_fg_color = DANGER;
    visuals.extreme_bg_color = INPUT_BG;
    visuals.text_edit_bg_color = Some(INPUT_BG);
    visuals.code_bg_color = SURFACE_ELEVATED;

    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);

    visuals.widgets.inactive.bg_fill = SURFACE_ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_ELEVATED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);

    visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_HI);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);

    visuals.widgets.active.bg_fill = SURFACE_ACTIVE;
    visuals.widgets.active.weak_bg_fill = SURFACE_ACTIVE;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.active.corner_radius = CornerRadius::same(8);

    visuals.widgets.open.bg_fill = SURFACE_ELEVATED;
    visuals.widgets.open.weak_bg_fill = SURFACE_ELEVATED;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, BORDER_HI);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.open.corner_radius = CornerRadius::same(8);

    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
        style.spacing.interact_size = Vec2::new(40.0, 30.0);
        style.spacing.slider_width = 160.0;
        style.spacing.combo_width = 160.0;
    });
}

fn load_prefs() -> Prefs {
    let Some(dir) = util::config_dir() else {
        return Prefs::default();
    };
    std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn load_logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([256, 256], WINDOW_ICON);
    ctx.load_texture("screenrec_logo", image, egui::TextureOptions::LINEAR)
}

fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let short: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

fn fmt_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

const WINDOW_ICON: &[u8] = include_bytes!("../resources/icon_256.rgba");

fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: WINDOW_ICON.to_vec(),
        width: 256,
        height: 256,
    }
}

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ScreenRec")
            .with_app_id("screen_rec")
            .with_icon(window_icon())
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([960.0, 660.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "ScreenRec",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
}
