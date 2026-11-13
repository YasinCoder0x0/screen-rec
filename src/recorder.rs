use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use parking_lot::Mutex;
use windows_capture::capture::{CaptureControl, GraphicsCaptureApiError, GraphicsCaptureApiHandler};
use windows_capture::encoder::VideoEncoder;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, GraphicsCaptureItemType,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::audio::{self, AudioHandles};
use crate::capture::{
    CaptureFlags, CaptureHandler, GrabFlags, GrabFrame, GrabHandler, ScreenshotFlags,
    ScreenshotHandler,
};
pub use crate::capture::GrabFrame as PreviewFrame;
use crate::webcam::{self, WebcamOptions, WebcamStream};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type SharedEncoder = Arc<Mutex<Option<VideoEncoder>>>;
pub type SharedError = Arc<Mutex<Option<String>>>;
type Control = CaptureControl<CaptureHandler, BoxError>;
type ScreenshotControl = CaptureControl<ScreenshotHandler, BoxError>;
type GrabControl = CaptureControl<GrabHandler, BoxError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn is_valid(&self) -> bool {
        self.width > 1 && self.height > 1
    }

    /// Clamp the region to the capture surface and make it encoder friendly
    /// (even width/height).
    pub fn normalized(&self, max_width: u32, max_height: u32) -> Option<Region> {
        if !self.is_valid() {
            return None;
        }
        let x = self.x.min(max_width.saturating_sub(2));
        let y = self.y.min(max_height.saturating_sub(2));
        let width = self.width.min(max_width.saturating_sub(x)) & !1;
        let height = self.height.min(max_height.saturating_sub(y)) & !1;
        if width < 2 || height < 2 {
            return None;
        }
        Some(Region {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CaptureTarget {
    Monitor(Monitor),
    Window(Window),
}

impl CaptureTarget {
    pub fn width(&self) -> u32 {
        match self {
            CaptureTarget::Monitor(m) => m.width().unwrap_or(0),
            CaptureTarget::Window(w) => w.width().unwrap_or(0).max(0) as u32,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            CaptureTarget::Monitor(m) => m.height().unwrap_or(0),
            CaptureTarget::Window(w) => w.height().unwrap_or(0).max(0) as u32,
        }
    }

    pub fn label(&self) -> String {
        match self {
            CaptureTarget::Monitor(m) => {
                let name = m.name().unwrap_or_default();
                let name = if name.trim().is_empty() {
                    let idx = m.index().unwrap_or(1);
                    format!("Display {idx}")
                } else {
                    name
                };
                let hz = m.refresh_rate().unwrap_or(60);
                format!("{name} ({}x{} @ {hz}Hz)", self.width(), self.height())
            }
            CaptureTarget::Window(w) => {
                let title = w.title().unwrap_or_else(|_| "Window".to_string());
                let title = if title.trim().is_empty() {
                    "Window".to_string()
                } else {
                    title
                };
                let proc = w.process_name().unwrap_or_default();
                if proc.is_empty() {
                    format!("{title} ({}x{})", self.width(), self.height())
                } else {
                    format!("{proc} · {title} ({}x{})", self.width(), self.height())
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecordingOptions {
    pub target: CaptureTarget,
    pub output: PathBuf,
    pub fps: u32,
    pub bitrate: u32,
    pub codec: Codec,
    pub system_audio: bool,
    pub microphone: bool,
    pub capture_cursor: bool,
    pub region: Option<Region>,
    pub webcam: Option<WebcamOptions>,
}

impl RecordingOptions {
    pub fn with_defaults(target: CaptureTarget) -> Self {
        let dir = crate::util::recordings_dir();
        Self {
            target,
            output: crate::util::timestamped_path(&dir, "mp4"),
            fps: 60,
            bitrate: 12_000_000,
            codec: Codec::H264,
            system_audio: true,
            microphone: false,
            capture_cursor: true,
            region: None,
            webcam: None,
        }
    }
}

pub struct ActiveRecording {
    control: Option<Control>,
    audio: Option<AudioHandles>,
    encoder: SharedEncoder,
    error: SharedError,
    frames: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    audio_level: Arc<AtomicU32>,
    started: Instant,
    output_path: PathBuf,
}

impl ActiveRecording {
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    pub fn file_size(&self) -> u64 {
        std::fs::metadata(&self.output_path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    pub fn is_capture_alive(&self) -> bool {
        self.control.as_ref().is_some_and(|control| !control.is_finished())
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn audio_level(&self) -> u32 {
        self.audio_level.load(Ordering::Relaxed)
    }

    pub fn stop(mut self) -> Result<PathBuf> {
        self.finalize()?;
        Ok(self.output_path.clone())
    }

    fn finalize(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);

        if let Some(audio) = self.audio.take() {
            audio.stop_and_join();
        }

        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }

        if let Some(encoder) = self.encoder.lock().take() {
            finish_encoder(encoder)?;
        }

        Ok(())
    }
}

impl Drop for ActiveRecording {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

fn finish_encoder(encoder: VideoEncoder) -> Result<()> {
    let handle = std::thread::Builder::new()
        .name("screenrec-finalize".to_string())
        .spawn(move || {
            let _ = wasapi::initialize_mta();
            encoder.finish().map_err(|err| anyhow::anyhow!("{err}"))
        })
        .context("failed to spawn finalizer thread")?;

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("encoder finalizer panicked"))?
}

pub fn start(options: RecordingOptions) -> Result<ActiveRecording> {
    let target_width = options.target.width();
    let target_height = options.target.height();

    if target_width == 0 || target_height == 0 {
        bail!("the selected capture target has no size");
    }

    let region = match options.region {
        Some(region) => Some(
            region
                .normalized(target_width, target_height)
                .ok_or_else(|| anyhow::anyhow!("the selected capture region is invalid"))?,
        ),
        None => None,
    };

    let width = region.map(|r| r.width).unwrap_or(target_width);
    let height = region.map(|r| r.height).unwrap_or(target_height);

    if let Some(parent) = options.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let encoder: SharedEncoder = Arc::new(Mutex::new(None));
    let error: SharedError = Arc::new(Mutex::new(None));
    let frames = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let audio_level = Arc::new(AtomicU32::new(0));

    let audio_enabled = options.system_audio || options.microphone;

    let camera: Option<WebcamStream> = match &options.webcam {
        Some(cam_options) => match webcam::start(cam_options.clone()) {
            Ok(stream) => Some(stream),
            Err(err) => {
                *error.lock() = Some(format!("Webcam unavailable: {err:#}"));
                None
            }
        },
        None => None,
    };

    let flags = CaptureFlags {
        encoder_slot: encoder.clone(),
        error_slot: error.clone(),
        frames: frames.clone(),
        stop: stop.clone(),
        paused: paused.clone(),
        width,
        height,
        fps: options.fps,
        bitrate: options.bitrate,
        audio: audio_enabled,
        codec: options.codec,
        output_path: options.output.clone(),
        region,
        webcam: camera.clone(),
    };

    let control = start_control(options.target, flags, options.capture_cursor)?;

    if encoder.lock().is_none() {
        bail!("the video encoder failed to initialize");
    }

    let audio = if audio_enabled {
        Some(audio::start(
            encoder.clone(),
            error.clone(),
            options.system_audio,
            options.microphone,
            paused.clone(),
            audio_level.clone(),
        ))
    } else {
        None
    };

    Ok(ActiveRecording {
        control: Some(control),
        audio,
        encoder,
        error,
        frames,
        stop,
        paused,
        audio_level,
        started: Instant::now(),
        output_path: options.output,
    })
}

fn start_control(target: CaptureTarget, flags: CaptureFlags, cursor: bool) -> Result<Control> {
    let cursor_setting = if cursor {
        CursorCaptureSettings::WithCursor
    } else {
        CursorCaptureSettings::WithoutCursor
    };

    match target {
        CaptureTarget::Monitor(monitor) => start_with_fallback(monitor, flags, cursor_setting),
        CaptureTarget::Window(window) => start_with_fallback(window, flags, cursor_setting),
    }
}

fn start_with_fallback<T>(
    item: T,
    flags: CaptureFlags,
    cursor: CursorCaptureSettings,
) -> Result<Control>
where
    T: Copy + TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let attempt = |cursor: CursorCaptureSettings,
                   border: DrawBorderSettings|
     -> std::result::Result<Control, GraphicsCaptureApiError<BoxError>> {
        CaptureHandler::start_free_threaded(Settings::new(
            item,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags.clone(),
        ))
    };

    match attempt(cursor, DrawBorderSettings::WithoutBorder) {
        Ok(control) => Ok(control),
        Err(first) => attempt(CursorCaptureSettings::Default, DrawBorderSettings::Default)
            .map_err(|second| anyhow::anyhow!("capture failed: {first} / fallback: {second}")),
    }
}

/// Captures a single frame of the target (optionally cropped to a region) and
/// writes it to `output` as a PNG.
pub fn screenshot(target: CaptureTarget, region: Option<Region>, output: PathBuf) -> Result<PathBuf> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let result: Arc<Mutex<Option<std::result::Result<PathBuf, String>>>> =
        Arc::new(Mutex::new(None));

    let region = region.and_then(|r| {
        r.normalized(target.width(), target.height())
    });

    let flags = ScreenshotFlags {
        output: output.clone(),
        region,
        result: result.clone(),
    };

    let control = start_screenshot_control(target, flags)?;
    let _ = control.wait();

    let outcome = result.lock().take();
    match outcome {
        Some(Ok(path)) => Ok(path),
        Some(Err(err)) => bail!("{err}"),
        None => bail!("the screenshot could not be captured"),
    }
}

fn start_screenshot_control(
    target: CaptureTarget,
    flags: ScreenshotFlags,
) -> Result<ScreenshotControl> {
    match target {
        CaptureTarget::Monitor(monitor) => screenshot_with_fallback(monitor, flags),
        CaptureTarget::Window(window) => screenshot_with_fallback(window, flags),
    }
}

fn screenshot_with_fallback<T>(item: T, flags: ScreenshotFlags) -> Result<ScreenshotControl>
where
    T: Copy + TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let attempt = |cursor: CursorCaptureSettings,
                   border: DrawBorderSettings|
     -> std::result::Result<ScreenshotControl, GraphicsCaptureApiError<BoxError>> {
        ScreenshotHandler::start_free_threaded(Settings::new(
            item,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags.clone(),
        ))
    };

    match attempt(CursorCaptureSettings::WithoutCursor, DrawBorderSettings::WithoutBorder) {
        Ok(control) => Ok(control),
        Err(first) => attempt(CursorCaptureSettings::Default, DrawBorderSettings::Default)
            .map_err(|second| anyhow::anyhow!("screenshot failed: {first} / fallback: {second}")),
    }
}

/// Capture a single frame as a raw BGRA snapshot (used for UI previews).
pub fn grab(target: CaptureTarget, region: Option<Region>) -> Result<GrabFrame> {
    let result: Arc<Mutex<Option<std::result::Result<GrabFrame, String>>>> =
        Arc::new(Mutex::new(None));

    let region = region.and_then(|r| r.normalized(target.width(), target.height()));

    let flags = GrabFlags {
        region,
        result: result.clone(),
    };

    let control = start_grab_control(target, flags)?;
    let _ = control.wait();

    let outcome = result.lock().take();
    match outcome {
        Some(Ok(frame)) => Ok(frame),
        Some(Err(err)) => bail!("{err}"),
        None => bail!("the preview could not be captured"),
    }
}

fn start_grab_control(target: CaptureTarget, flags: GrabFlags) -> Result<GrabControl> {
    match target {
        CaptureTarget::Monitor(monitor) => grab_with_fallback(monitor, flags),
        CaptureTarget::Window(window) => grab_with_fallback(window, flags),
    }
}

fn grab_with_fallback<T>(item: T, flags: GrabFlags) -> Result<GrabControl>
where
    T: Copy + TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let attempt = |cursor: CursorCaptureSettings,
                   border: DrawBorderSettings|
     -> std::result::Result<GrabControl, GraphicsCaptureApiError<BoxError>> {
        GrabHandler::start_free_threaded(Settings::new(
            item,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags.clone(),
        ))
    };

    match attempt(CursorCaptureSettings::WithoutCursor, DrawBorderSettings::WithoutBorder) {
        Ok(control) => Ok(control),
        Err(first) => attempt(CursorCaptureSettings::Default, DrawBorderSettings::Default)
            .map_err(|second| anyhow::anyhow!("preview failed: {first} / fallback: {second}")),
    }
}

pub fn set_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
