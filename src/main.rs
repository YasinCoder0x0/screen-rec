mod audio;
mod capture;
mod gui;
mod history;
mod hotkey;
mod monitor_util;
mod recorder;
mod util;
mod webcam;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use recorder::{CaptureTarget, Codec, RecordingOptions, Region};
use windows_capture::monitor::Monitor;
use windows_capture::window::Window;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct Args {
    headless: bool,
    seconds: u64,
    out: Option<PathBuf>,
    fps: Option<u32>,
    bitrate: Option<u32>,
    monitor: Option<usize>,
    window: Option<String>,
    region: Option<Region>,
    no_audio: bool,
    mic: bool,
    no_cursor: bool,
    hevc: bool,
    help: bool,
    version: bool,
}

fn print_help() {
    println!(
        r#"ScreenRec {VERSION} - Fast, native, high-quality Windows screen recorder

USAGE:
    ScreenRec.exe [OPTIONS]

Running without options opens the graphical interface.

OPTIONS:
    --headless, record         Run without GUI (records immediately)
    --seconds <SECS>           Record for a fixed duration, then stop automatically
    --out <PATH>               Custom output file path (.mp4)
    --fps <N>                  Frames per second (24, 30, 60, 120; default: 60)
    --bitrate <BPS>            Video bitrate in bits per second (default: adaptive)
    --monitor <INDEX>          Monitor index (1-based; default: 1)
    --window <TITLE>           Capture the window containing this title substring
    --region <X,Y,W,H>         Capture only this rectangle of the target
    --no-audio                 Disable system audio recording
    --mic                      Enable microphone recording
    --no-cursor                Do not capture the mouse cursor
    --hevc                     Encode with HEVC / H.265 instead of H.264
    -V, --version              Print the version and exit
    -h, --help                 Print this help information

GLOBAL SHORTCUTS (in GUI mode):
    F9                         Start / Stop recording
    F10                        Pause / Resume recording
    F11                        Take a screenshot
"#
    );
}

fn parse_region(text: &str) -> Option<Region> {
    let parts: Vec<u32> = text
        .split([',', 'x', 'X'])
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    if parts.len() == 4 {
        Some(Region {
            x: parts[0],
            y: parts[1],
            width: parts[2],
            height: parts[3],
        })
    } else {
        None
    }
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => args.help = true,
            "-V" | "--version" => args.version = true,
            "--headless" | "record" => args.headless = true,
            "--seconds" => args.seconds = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--out" => args.out = iter.next().map(PathBuf::from),
            "--fps" => args.fps = iter.next().and_then(|v| v.parse().ok()),
            "--bitrate" => args.bitrate = iter.next().and_then(|v| v.parse().ok()),
            "--monitor" => args.monitor = iter.next().and_then(|v| v.parse().ok()),
            "--window" => args.window = iter.next(),
            "--region" => args.region = iter.next().as_deref().and_then(parse_region),
            "--no-audio" => args.no_audio = true,
            "--mic" => args.mic = true,
            "--no-cursor" => args.no_cursor = true,
            "--hevc" => args.hevc = true,
            _ => {}
        }
    }

    args
}

impl Args {
    fn wants_headless(&self) -> bool {
        self.headless
            || self.seconds > 0
            || self.out.is_some()
            || self.fps.is_some()
            || self.bitrate.is_some()
            || self.monitor.is_some()
            || self.window.is_some()
            || self.region.is_some()
            || self.mic
            || self.no_audio
            || self.no_cursor
            || self.hevc
    }
}

fn main() {
    let args = parse_args();

    if args.help {
        attach_console();
        print_help();
        return;
    }
    if args.version {
        attach_console();
        println!("ScreenRec {VERSION}");
        return;
    }

    let gui_mode = !args.wants_headless();
    if gui_mode {
        hide_own_console();
    } else {
        attach_console();
    }

    if let Err(err) = run(args) {
        let message = format!("{err:#}");
        eprintln!("error: {message}");
        if gui_mode {
            show_error_dialog(&message);
        }
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{
        AllocConsole, AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS,
    };
    unsafe {
        if !GetConsoleWindow().0.is_null() {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {}

#[cfg(windows)]
fn hide_own_console() {
    use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    unsafe {
        let hwnd = GetConsoleWindow();
        if hwnd.0.is_null() {
            return;
        }
        let mut pids = [0u32; 2];
        if GetConsoleProcessList(&mut pids) == 1 {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn hide_own_console() {}

#[cfg(windows)]
fn show_error_dialog(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    let text = HSTRING::from(message);
    let caption = HSTRING::from("ScreenRec - Startup error");
    unsafe {
        MessageBoxW(None, &text, &caption, MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn show_error_dialog(message: &str) {
    eprintln!("{message}");
}

fn run(args: Args) -> Result<()> {
    recorder::set_dpi_awareness();

    if args.wants_headless() {
        run_headless(&args)
    } else {
        gui::run()
    }
}

fn build_target(args: &Args) -> Result<CaptureTarget> {
    if let Some(title) = &args.window {
        let window = Window::from_contains_name(title)
            .with_context(|| format!("no window title contains \"{title}\""))?;
        return Ok(CaptureTarget::Window(window));
    }

    let index = args.monitor.unwrap_or(1);
    let monitor = Monitor::from_index(index)
        .or_else(|_| Monitor::from_index(1))
        .context("no monitor available")?;
    Ok(CaptureTarget::Monitor(monitor))
}

fn run_headless(args: &Args) -> Result<()> {
    let target = build_target(args)?;

    let mut options = RecordingOptions::with_defaults(target);
    if let Some(out) = &args.out {
        options.output = out.clone();
    }
    if let Some(fps) = args.fps {
        options.fps = fps.clamp(1, 240);
    }
    if let Some(bitrate) = args.bitrate {
        options.bitrate = bitrate.max(100_000);
    }
    options.system_audio = !args.no_audio;
    options.microphone = args.mic;
    options.capture_cursor = !args.no_cursor;
    options.codec = if args.hevc { Codec::Hevc } else { Codec::H264 };
    if let Some(region) = args.region {
        options.region = Some(region);
    }

    println!(
        "Recording {} @ {} fps, {} ({}) -> {}",
        options.target.label(),
        options.fps,
        options.codec.label(),
        util::human_size(options.bitrate as u64 / 8) + "/s",
        options.output.display()
    );

    let recording = recorder::start(options)?;
    if args.seconds > 0 {
        println!("Recording started. Stopping automatically in {}s...", args.seconds);
    } else {
        println!("Recording started. Press Enter to stop...");
    }

    let start = Instant::now();
    let mut last_report = Instant::now();

    // Spawn a thread to wait for Enter key if seconds == 0
    let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
    if args.seconds == 0 {
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            let _ = stop_tx.send(());
        });
    }

    loop {
        if let Some(err) = recording.error() {
            eprintln!("warning: {err}");
        }

        if !recording.is_capture_alive() {
            break;
        }

        if args.seconds > 0 && start.elapsed() >= Duration::from_secs(args.seconds) {
            break;
        }

        if args.seconds == 0 && stop_rx.try_recv().is_ok() {
            break;
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            println!(
                "  {:.1}s | {} frames | {}",
                recording.elapsed().as_secs_f64(),
                recording.frames(),
                util::human_size(recording.file_size())
            );
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    std::thread::sleep(Duration::from_millis(150));
    let path = recording.stop()?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    println!("Saved {} ({})", path.display(), util::human_size(size));
    Ok(())
}
