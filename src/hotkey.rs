use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleRecord,
    TogglePause,
    Screenshot,
}

extern "system" {
    fn GetAsyncKeyState(v_key: i32) -> i16;
}

const VK_F9: i32 = 0x78;
const VK_F10: i32 = 0x79;
const VK_F11: i32 = 0x7A;

pub struct HotkeyListener {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HotkeyListener {
    pub fn new() -> (Self, Receiver<HotkeyAction>) {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let join = thread::Builder::new()
            .name("screenrec-hotkeys".to_string())
            .spawn(move || run_listener(tx, stop_clone))
            .ok();

        (Self { stop, join }, rx)
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_listener(tx: Sender<HotkeyAction>, stop: Arc<AtomicBool>) {
    let mut f9_was_down = false;
    let mut f10_was_down = false;
    let mut f11_was_down = false;

    while !stop.load(Ordering::Relaxed) {
        let f9_state = unsafe { GetAsyncKeyState(VK_F9) };
        let f9_down = (f9_state as u16 & 0x8000) != 0;
        if f9_down && !f9_was_down {
            let _ = tx.send(HotkeyAction::ToggleRecord);
        }
        f9_was_down = f9_down;

        let f10_state = unsafe { GetAsyncKeyState(VK_F10) };
        let f10_down = (f10_state as u16 & 0x8000) != 0;
        if f10_down && !f10_was_down {
            let _ = tx.send(HotkeyAction::TogglePause);
        }
        f10_was_down = f10_down;

        let f11_state = unsafe { GetAsyncKeyState(VK_F11) };
        let f11_down = (f11_state as u16 & 0x8000) != 0;
        if f11_down && !f11_was_down {
            let _ = tx.send(HotkeyAction::Screenshot);
        }
        f11_was_down = f11_down;

        thread::sleep(Duration::from_millis(40));
    }
}
