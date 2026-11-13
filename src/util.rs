use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn recordings_dir() -> PathBuf {
    home_dir().join("Videos").join("ScreenRec")
}

pub fn screenshots_dir() -> PathBuf {
    home_dir().join("Pictures").join("ScreenRec")
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("ScreenRec"))
        .or_else(|| Some(home_dir().join(".config").join("screen_rec")))
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn timestamped_path(dir: &Path, ext: &str) -> PathBuf {
    let (y, m, d, hh, mm, ss, ms) = local_parts();
    dir.join(format!(
        "ScreenRec_{y:04}-{m:02}-{d:02}_{hh:02}-{mm:02}-{ss:02}_{ms:03}.{ext}"
    ))
}

#[cfg(windows)]
fn local_parts() -> (u16, u16, u16, u16, u16, u16, u16) {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let now = unsafe { GetLocalTime() };
    (
        now.wYear,
        now.wMonth,
        now.wDay,
        now.wHour,
        now.wMinute,
        now.wSecond,
        now.wMilliseconds,
    )
}

#[cfg(not(windows))]
fn local_parts() -> (i64, u32, u32, u32, u32, u32, u32) {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000) as u32;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
        ms,
    )
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn format_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}_{hh:02}-{mm:02}-{ss:02}")
}

#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
