//! Ctrl+Alt+L marks "it lagged just now". It is a registered hotkey, which Windows delivers
//! as a message; no keyboard hook is installed.

use super::util::join_within;
use super::Shared;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_HOTKEY, WM_QUIT,
};

const ID: i32 = 1;
const KEY_L: u32 = 0x4C;

pub struct Hotkey {
    thread: JoinHandle<()>,
    thread_id: u32,
}

impl Hotkey {
    pub fn start(shared: Arc<Shared>) -> Result<Hotkey, String> {
        let (ready, registered) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("culprit hotkey".into())
            .spawn(move || {
                let mut msg = MSG::default();
                // SAFETY: message calls on this thread's own queue.
                unsafe {
                    // Make sure the thread has a message queue before anyone posts to it.
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    let ok = RegisterHotKey(None, ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, KEY_L).is_ok();
                    let _ = ready.send(ok.then(|| GetCurrentThreadId()));
                    if !ok {
                        return;
                    }
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        if msg.message == WM_HOTKEY {
                            let now = shared.now();
                            shared.with(|s| s.collector.mark(now));
                        }
                    }
                    let _ = UnregisterHotKey(None, ID);
                }
            })
            .map_err(|e| e.to_string())?;
        match registered.recv_timeout(Duration::from_secs(2)) {
            Ok(Some(thread_id)) => Ok(Hotkey { thread, thread_id }),
            _ => Err("the hotkey is taken".into()),
        }
    }

    pub fn stop(self) {
        // SAFETY: posts to the hotkey thread's queue, which exists until it exits.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        join_within(self.thread, Duration::from_secs(2));
    }
}
