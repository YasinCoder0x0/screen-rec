use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, CornerRadius, Margin, RichText, Stroke, Vec2};

use windows_capture::monitor::Monitor;
use windows_capture::window::Window;

use crate::history::{self, HistoryEntry};
use crate::hotkey::{HotkeyAction, HotkeyListener};
use crate::monitor_util::{self, MonitorBounds};
use crate::recorder::{self, ActiveRecording, CaptureTarget, Codec, RecordingOptions, Region};
use crate::util;
use crate::webcam::{self as camera, Corner, WebcamOptions};

const BG: Color32 = Color32::from_rgb(14, 15, 20);
const CARD: Color32 = Color32::from_rgb(24, 26, 34);
const CARD_HI: Color32 = Color32::from_rgb(31, 34, 44);
const BORDER: Color32 = Color32::from_rgb(45, 49, 62);
const TEXT: Color32 = Color32::from_rgb(228, 232, 240);
const MUTED: Color32 = Color32::from_rgb(148, 160, 178);
const ACCENT: Color32 = Color32::from_rgb(99, 102, 241);
const ACCENT_HI: Color32 = Color32::from_rgb(129, 140, 248);
const DANGER: Color32 = Color32::from_rgb(239, 68, 68);
const SUCCESS: Color32 = Color32::from_rgb(52, 211, 153);
const CAUTION: Color32 = Color32::from_rgb(251, 191, 36);

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
    const ALL: [Quality; 4] = [Quality::Low, Quality::Balanced, Quality::High, Quality::Ultra];

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
    Countdown {
        secs_left: u8,
        deadline: Instant,
    },
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

    fps_mark: Instant,
    fps_frames: u64,
    display_fps: f32,
    started_at: Option<Instant>,

    _hotkey_listener: Option<HotkeyListener>,
    hotkey_rx: Option<Receiver<HotkeyAction>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let prefs = load_prefs();

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
            codec: if prefs.codec == 1 { Codec::Hevc } else { Codec::H264 },
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
            fps_mark: Instant::now(),
            fps_frames: 0,
            display_fps: 0.0,
            started_at: None,
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
            Some(region.normalized(target.width(), target.height()).ok_or_else(|| {
                "The selected region is invalid. Pick a region or use the full display."
                    .to_string()
            })?)
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
        if matches!(self.task, Task::Starting(_) | Task::Stopping(_) | Task::PickingFolder(_)) {
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

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("screenrec-screenshot".to_string())
            .spawn(move || {
                let result = recorder::screenshot(target, region, path)
                    .map_err(|err| format!("{err:#}"));
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
                            x: (nx * picker.monitor.width as f32).round().clamp(0.0, picker.monitor.width as f32) as u32,
                            y: (ny * picker.monitor.height as f32).round().clamp(0.0, picker.monitor.height as f32) as u32,
                            width: ((nx2 - nx) * picker.monitor.width as f32).round().max(2.0) as u32,
                            height: ((ny2 - ny) * picker.monitor.height as f32).round().max(2.0) as u32,
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

            // Dim everything except the current selection so the desktop stays visible.
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
                        format!("{} x {}", sel.width().round() as i32, sel.height().round() as i32),
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
        // Start / stop the global hotkey listener when the preference changes.
        if self.hotkeys_enabled && self.hotkey_rx.is_none() {
            let (listener, rx) = HotkeyListener::new();
            self._hotkey_listener = Some(listener);
            self.hotkey_rx = Some(rx);
        } else if !self.hotkeys_enabled && self.hotkey_rx.is_some() {
            self.hotkey_rx = None;
            self._hotkey_listener = None;
        }

        // Poll global hotkeys if enabled
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

        // Auto-stop when the configured recording limit is reached.
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
            Task::Countdown { mut secs_left, deadline } => {
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
                    self.task = Task::Countdown { secs_left, deadline };
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
                    self.history =
                        history::add(self.history.clone(), HistoryEntry::new(&path, "video", duration));
                    self.last_saved = Some(path);
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
                    self.history =
                        history::add(self.history.clone(), HistoryEntry::new(&path, "screenshot", 0.0));
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
            target_mode: if self.mode == TargetMode::Window { 1 } else { 0 },
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

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("ScreenRec").size(24.0).strong().color(TEXT));
            ui.add_space(4.0);
            ui.label(
                RichText::new("pro · minimal · hardware accelerated")
                    .size(11.0)
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("F9: Rec · F10: Pause")
                        .size(11.0)
                        .color(ACCENT_HI),
                );
            });
        });
    }

    fn source_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "Capture Source");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, TargetMode::Monitor, "Display");
                ui.selectable_value(&mut self.mode, TargetMode::Window, "Window");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Refresh").size(12.0))
                                .fill(CARD_HI)
                                .corner_radius(CornerRadius::same(7)),
                        )
                        .clicked()
                    {
                        self.refresh_sources();
                    }
                });
            });

            ui.add_space(6.0);

            match self.mode {
                TargetMode::Monitor => {
                    let labels: Vec<String> =
                        self.monitors.iter().map(|m| m.label.clone()).collect();
                    let current = labels
                        .get(self.selected_monitor)
                        .cloned()
                        .unwrap_or_else(|| "No displays detected".to_string());

                    egui::ComboBox::from_id_salt("monitor")
                        .selected_text(RichText::new(current).color(TEXT))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            if labels.is_empty() {
                                ui.label("No displays detected");
                            }
                            for (index, label) in labels.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    index,
                                    label.as_str(),
                                );
                            }
                        });
                }
                TargetMode::Window => {
                    let labels: Vec<String> =
                        self.windows.iter().map(|w| w.label.clone()).collect();
                    let current = labels
                        .get(self.selected_window)
                        .cloned()
                        .unwrap_or_else(|| "No windows detected".to_string());

                    egui::ComboBox::from_id_salt("window")
                        .selected_text(RichText::new(current).color(TEXT))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            if labels.is_empty() {
                                ui.label("No visible windows found. Click Refresh.");
                            }
                            for (index, label) in labels.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.selected_window,
                                    index,
                                    label.as_str(),
                                );
                            }
                        });
                }
            }

            if self.mode == TargetMode::Monitor {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.region_enabled, "Record a region");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Select region").size(12.0))
                                    .fill(CARD_HI)
                                    .corner_radius(CornerRadius::same(7)),
                            )
                            .clicked()
                        {
                            self.start_region_picker();
                        }
                    });
                });

                if self.region_enabled {
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

                    ui.horizontal(|ui| {
                        ui.label(RichText::new("X").size(11.0).color(MUTED));
                        ui.add(
                            egui::DragValue::new(&mut self.region_x)
                                .range(0..=max_w)
                                .speed(1.0),
                        );
                        ui.label(RichText::new("Y").size(11.0).color(MUTED));
                        ui.add(
                            egui::DragValue::new(&mut self.region_y)
                                .range(0..=max_h)
                                .speed(1.0),
                        );
                        ui.label(RichText::new("W").size(11.0).color(MUTED));
                        ui.add(
                            egui::DragValue::new(&mut self.region_width)
                                .range(2..=max_w.max(2))
                                .speed(1.0),
                        );
                        ui.label(RichText::new("H").size(11.0).color(MUTED));
                        ui.add(
                            egui::DragValue::new(&mut self.region_height)
                                .range(2..=max_h.max(2))
                                .speed(1.0),
                        );
                    });

                    if self.region_width <= 1 || self.region_height <= 1 {
                        ui.label(
                            RichText::new("Click \"Select region\" and drag over the display.")
                                .size(11.0)
                                .color(CAUTION),
                        );
                    }
                }
            }
        });
    }

    fn video_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "Video Settings");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new("Frame rate").color(MUTED));
                ui.add_space(6.0);
                for fps in [24u32, 30, 60, 120] {
                    if ui
                        .selectable_label(self.fps == fps, format!("{fps} fps"))
                        .clicked()
                    {
                        self.fps = fps;
                    }
                }
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new("Quality").color(MUTED));
                ui.add_space(6.0);
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(self.quality.label())
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for quality in Quality::ALL {
                            ui.selectable_value(&mut self.quality, quality, quality.label());
                        }
                    });

                ui.add_space(12.0);
                ui.label(RichText::new("Codec").color(MUTED));
                ui.add_space(6.0);
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(self.codec.label())
                    .width(95.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.codec, Codec::H264, "H.264");
                        ui.selectable_value(&mut self.codec, Codec::Hevc, "HEVC");
                    });
            });
        });
    }

    fn audio_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "Audio & Input");
            ui.add_space(6.0);
            ui.checkbox(&mut self.system_audio, "System audio (speakers / headset)");
            ui.add_space(2.0);
            ui.checkbox(&mut self.microphone, "Microphone (voice input)");
            ui.add_space(2.0);
            ui.checkbox(&mut self.capture_cursor, "Capture mouse cursor");
        });
    }

    fn webcam_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "Webcam Overlay");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.webcam_enabled, "Show webcam bubble");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Refresh").size(12.0))
                                .fill(CARD_HI)
                                .corner_radius(CornerRadius::same(7)),
                        )
                        .clicked()
                    {
                        self.refresh_webcams();
                    }
                });
            });

            if self.webcam_enabled {
                if self.webcam_devices.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("No camera detected. Connect a camera and press Refresh.")
                            .size(11.0)
                            .color(CAUTION),
                    );
                } else {
                    let labels = self.webcam_devices.clone();
                    let current = labels
                        .get(self.webcam_device)
                        .cloned()
                        .unwrap_or_else(|| "Camera".to_string());

                    ui.add_space(4.0);
                    egui::ComboBox::from_id_salt("webcam_device")
                        .selected_text(RichText::new(truncate(&current, 32)).color(TEXT))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (index, label) in labels.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.webcam_device,
                                    index,
                                    truncate(label, 48),
                                );
                            }
                        });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Size").color(MUTED));
                        ui.add(
                            egui::Slider::new(&mut self.webcam_size, 8..=60)
                                .suffix("%")
                                .show_value(true),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Corner").color(MUTED));
                        egui::ComboBox::from_id_salt("webcam_corner")
                            .selected_text(self.webcam_corner.label())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for corner in Corner::ALL {
                                    ui.selectable_value(
                                        &mut self.webcam_corner,
                                        corner,
                                        corner.label(),
                                    );
                                }
                            });
                        ui.add_space(8.0);
                        ui.checkbox(&mut self.webcam_mirror, "Mirror");
                    });
                }
            }
        });
    }

    fn preferences_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "Recording Preferences");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new("Countdown:").color(MUTED));
                ui.add_space(4.0);
                if ui.selectable_label(self.countdown_secs == 0, "None").clicked() {
                    self.countdown_secs = 0;
                }
                if ui.selectable_label(self.countdown_secs == 3, "3s").clicked() {
                    self.countdown_secs = 3;
                }
                if ui.selectable_label(self.countdown_secs == 5, "5s").clicked() {
                    self.countdown_secs = 5;
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Auto-stop:").color(MUTED));
                for (secs, label) in [
                    (0u64, "Off"),
                    (60, "1m"),
                    (300, "5m"),
                    (600, "10m"),
                    (1800, "30m"),
                    (3600, "60m"),
                ] {
                    if ui
                        .selectable_label(self.max_duration_secs == secs, label)
                        .clicked()
                    {
                        self.max_duration_secs = secs;
                    }
                }
            });

            ui.add_space(4.0);
            ui.checkbox(&mut self.minimize_on_record, "Minimize window when recording starts");
            ui.add_space(2.0);
            ui.checkbox(&mut self.hotkeys_enabled, "Enable global hotkeys (F9 / F10 / F11)");
        });
    }

    fn output_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "Destination Folder");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 148.0).max(80.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.output_dir)
                        .desired_width(width)
                        .text_color(TEXT),
                );
                if ui
                    .add(
                        egui::Button::new(RichText::new("Browse...").size(12.0))
                            .fill(CARD_HI)
                            .corner_radius(CornerRadius::same(7)),
                    )
                    .clicked()
                {
                    self.browse_output_folder();
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new("Open").size(12.0))
                            .fill(CARD_HI)
                            .corner_radius(CornerRadius::same(7)),
                    )
                    .clicked()
                {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(self.output_dir.trim())
                        .spawn();
                }
            });
        });
    }

    fn history_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Recent Recordings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Open folder").size(12.0))
                                .fill(CARD_HI)
                                .corner_radius(CornerRadius::same(7)),
                        )
                        .clicked()
                    {
                        let _ = std::process::Command::new("explorer.exe")
                            .arg(self.output_dir.trim())
                            .spawn();
                    }
                });
            });
            ui.add_space(6.0);

            let entries: Vec<HistoryEntry> = self.history.iter().take(6).cloned().collect();
            if entries.is_empty() {
                ui.label(
                    RichText::new("Recordings and screenshots will appear here.")
                        .size(11.0)
                        .color(MUTED),
                );
                return;
            }

            let mut request_delete: Option<String> = None;
            let mut confirm_delete: Option<String> = None;
            let mut cancel_delete = false;

            for entry in &entries {
                let path = entry.path_buf();
                let is_pending = self.pending_delete.as_deref() == Some(entry.path.as_str());
                ui.horizontal(|ui| {
                    let icon = if entry.kind == "screenshot" { "🖼" } else { "🎬" };
                    ui.label(RichText::new(icon).size(14.0).color(TEXT));
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(truncate(&entry.file_name(), 30))
                                .size(12.0)
                                .color(TEXT),
                        );
                        let detail = if entry.kind == "screenshot" {
                            util::human_size(entry.size)
                        } else {
                            format!(
                                "{} · {}",
                                util::human_duration(entry.duration_secs.round() as u64),
                                util::human_size(entry.size)
                            )
                        };
                        ui.label(RichText::new(detail).size(10.0).color(MUTED));
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if is_pending {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Delete").size(11.0))
                                        .fill(DANGER)
                                        .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                confirm_delete = Some(entry.path.clone());
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Cancel").size(11.0))
                                        .fill(CARD_HI)
                                        .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                cancel_delete = true;
                            }
                        } else {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Delete").size(11.0))
                                        .fill(CARD_HI)
                                        .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                request_delete = Some(entry.path.clone());
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Folder").size(11.0))
                                        .fill(CARD_HI)
                                        .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                let _ = std::process::Command::new("explorer.exe")
                                    .arg(format!("/select,{}", path.display()))
                                    .spawn();
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Play").size(11.0))
                                        .fill(ACCENT)
                                        .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                let _ = std::process::Command::new("explorer.exe")
                                    .arg(&path)
                                    .spawn();
                            }
                        }
                    });
                });
                ui.add_space(2.0);
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

    fn controls(&mut self, ui: &mut egui::Ui) {
        let busy = !matches!(self.task, Task::Idle);

        if let Some(recording) = &self.recording {
            let elapsed = recording.elapsed();
            let size = util::human_size(recording.file_size());
            let is_paused = recording.is_paused();

            ui.horizontal(|ui| {
                let (badge_text, badge_color) = if is_paused {
                    ("PAUSED", CAUTION)
                } else {
                    ("REC", DANGER)
                };
                ui.label(RichText::new(badge_text).strong().color(badge_color));
                let limit = if self.max_duration_secs > 0 {
                    format!(
                        "  ·  {} left",
                        fmt_duration(Duration::from_secs(
                            self.max_duration_secs.saturating_sub(elapsed.as_secs())
                        ))
                    )
                } else {
                    String::new()
                };
                ui.label(
                    RichText::new(format!(
                        "{}  ·  {:.0} fps  ·  {}{}",
                        fmt_duration(elapsed),
                        self.display_fps,
                        size,
                        limit
                    ))
                    .color(TEXT),
                );
            });

            // Live Audio Level Visualizer
            let level = recording.audio_level();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Audio:").size(11.0).color(MUTED));
                let (rect, _) = ui.allocate_exact_size(Vec2::new(100.0, 7.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(3), CARD_HI);
                let fill_pct = (level as f32 / 100.0).clamp(0.0, 1.0);
                if fill_pct > 0.01 {
                    let mut fill_rect = rect;
                    fill_rect.set_width(rect.width() * fill_pct);
                    let bar_color = if fill_pct > 0.85 {
                        DANGER
                    } else if fill_pct > 0.6 {
                        CAUTION
                    } else {
                        SUCCESS
                    };
                    ui.painter().rect_filled(fill_rect, CornerRadius::same(3), bar_color);
                }
                ui.label(RichText::new(format!("{level}%")).size(10.0).color(MUTED));
            });

            if let Some(warning) = recording.error() {
                ui.add_space(2.0);
                ui.label(RichText::new(warning).size(12.0).color(CAUTION));
            }
            ui.add_space(8.0);

            // Recording Action Buttons (Pause / Stop / Screenshot)
            let mut toggle_pause = false;
            let mut stop_clicked = false;
            let mut screenshot_clicked = false;

            ui.horizontal(|ui| {
                let pause_text = if is_paused { "▶ Resume (F10)" } else { "⏸ Pause (F10)" };
                let pause_color = if is_paused { SUCCESS } else { CARD_HI };

                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(pause_text)
                                .size(15.0)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(pause_color)
                        .corner_radius(CornerRadius::same(10))
                        .min_size(Vec2::new(130.0, 48.0)),
                    )
                    .clicked()
                {
                    toggle_pause = true;
                }

                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("⏹ Stop (F9)")
                                .size(15.0)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(DANGER)
                        .corner_radius(CornerRadius::same(10))
                        .min_size(Vec2::new(ui.available_width(), 48.0)),
                    )
                    .clicked()
                {
                    stop_clicked = true;
                }
            });

            ui.add_space(6.0);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("📸 Screenshot (F11)")
                            .size(13.0)
                            .color(TEXT),
                    )
                    .fill(CARD_HI)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(ui.available_width(), 32.0)),
                )
                .clicked()
            {
                screenshot_clicked = true;
            }

            if toggle_pause {
                if is_paused {
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
        } else if let Task::Countdown { secs_left, .. } = self.task {
            let cancel_btn = egui::Button::new(
                RichText::new(format!("⏱ Starting in {secs_left}... (Click to cancel)"))
                    .size(16.0)
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(CAUTION)
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(ui.available_width(), 48.0));

            if ui.add(cancel_btn).clicked() {
                self.task = Task::Idle;
            }
        } else {
            if busy {
                let label = match self.task {
                    Task::Starting(_) => "Initializing hardware encoder...",
                    Task::Stopping(_) => "Finalizing MP4 file...",
                    Task::PickingFolder(_) => "Selecting output folder...",
                    Task::Screenshot(_) => "Capturing screenshot...",
                    _ => "",
                };
                ui.label(RichText::new(label).color(MUTED));
                ui.add_space(8.0);
            }

            let button = egui::Button::new(
                RichText::new("⏺ Start recording (F9)")
                    .size(17.0)
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(if busy { CARD_HI } else { ACCENT })
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(ui.available_width(), 48.0));

            if ui.add_enabled(!busy, button).clicked() {
                self.request_start();
            }

            let mut screenshot_clicked = false;
            ui.add_space(6.0);
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(
                        RichText::new("📸 Take screenshot (F11)")
                            .size(13.0)
                            .color(TEXT),
                    )
                    .fill(CARD_HI)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(ui.available_width(), 34.0)),
                )
                .clicked()
            {
                screenshot_clicked = true;
            }
            if screenshot_clicked {
                self.take_screenshot();
            }

            if !busy {
                let hint = if self.mode == TargetMode::Window && self.windows.is_empty() {
                    "No windows available. Open an application, then press Refresh."
                } else {
                    "Hardware accelerated H.264/HEVC · Zero dropped frames"
                };
                ui.add_space(6.0);
                ui.label(RichText::new(hint).size(11.0).color(MUTED));
            }
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        if let Some(path) = &self.last_saved {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            ui.add_space(8.0);
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✓").color(SUCCESS).strong().size(16.0));
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Saved successfully").strong().color(TEXT),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} ({})",
                                truncate(&path.to_string_lossy(), 42),
                                util::human_size(size)
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                    });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("▶ Play").size(12.0).color(Color32::WHITE))
                                .fill(ACCENT)
                                .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        let _ = std::process::Command::new("explorer.exe")
                            .arg(path)
                            .spawn();
                    }
                    if ui
                        .add(
                            egui::Button::new(RichText::new("📁 Show in folder").size(12.0).color(TEXT))
                                .fill(CARD_HI)
                                .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        let _ = std::process::Command::new("explorer.exe")
                            .arg(format!("/select,{}", path.display()))
                            .spawn();
                    }
                });
            });
        }

        if let Some(error) = &self.error {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(Color32::from_rgb(46, 20, 24))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .stroke(Stroke::new(1.0, Color32::from_rgb(120, 40, 48)))
                .show(ui, |ui| {
                    ui.label(RichText::new(error).size(12.0).color(Color32::from_rgb(252, 165, 165)));
                });
        }
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

        // Repaint at ~15-20 fps when recording, counting down, or running background tasks
        if self.recording.is_some()
            || !matches!(self.task, Task::Idle)
            || self.region_picker.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(60));
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::same(18)),
            )
            .show(ui, |ui| {
                self.header(ui);
                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.source_card(ui);
                        ui.add_space(10.0);
                        self.video_card(ui);
                        ui.add_space(10.0);
                        self.audio_card(ui);
                        ui.add_space(10.0);
                        self.webcam_card(ui);
                        ui.add_space(10.0);
                        self.preferences_card(ui);
                        ui.add_space(10.0);
                        self.output_card(ui);
                        ui.add_space(12.0);
                        self.controls(ui);
                        self.history_card(ui);
                        self.footer(ui);
                    });
            });
    }
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(12))
        .stroke(Stroke::new(1.0, BORDER))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).strong().color(ACCENT_HI));
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.noninteractive.bg_fill = CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
    visuals.widgets.inactive.bg_fill = CARD_HI;
    visuals.widgets.inactive.weak_bg_fill = CARD_HI;
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 49, 64);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(45, 49, 64);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.corner_radius = CornerRadius::same(8);
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
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
            .with_inner_size([470.0, 710.0])
            .with_min_inner_size([430.0, 600.0])
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
