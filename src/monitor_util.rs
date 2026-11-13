use std::mem::size_of;

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

#[derive(Clone, Debug)]
pub struct MonitorBounds {
    pub device_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl MonitorBounds {
    pub fn logical_position(&self) -> (f32, f32) {
        (
            self.x as f32 / self.scale,
            self.y as f32 / self.scale,
        )
    }

    pub fn logical_size(&self) -> (f32, f32) {
        (
            self.width as f32 / self.scale,
            self.height as f32 / self.scale,
        )
    }
}

extern "system" fn enum_monitor(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<MonitorBounds>) };

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;

    let ok = unsafe {
        GetMonitorInfoW(
            hmonitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        )
    };
    if !ok.as_bool() {
        return TRUE;
    }

    let end = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let device_name = String::from_utf16_lossy(&info.szDevice[..end]);

    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    if unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_err() {
        dpi_x = 96;
    }
    let scale = if dpi_x == 0 { 1.0 } else { dpi_x as f32 / 96.0 };

    let rc = info.monitorInfo.rcMonitor;
    list.push(MonitorBounds {
        device_name,
        x: rc.left,
        y: rc.top,
        width: (rc.right - rc.left).max(0) as u32,
        height: (rc.bottom - rc.top).max(0) as u32,
        scale,
    });

    TRUE
}

pub fn enumerate() -> Vec<MonitorBounds> {
    let mut list: Vec<MonitorBounds> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list
}

pub fn find(device_name: &str) -> Option<MonitorBounds> {
    let monitors = enumerate();
    monitors
        .iter()
        .find(|m| m.device_name.eq_ignore_ascii_case(device_name))
        .cloned()
        .or_else(|| monitors.into_iter().next())
}
