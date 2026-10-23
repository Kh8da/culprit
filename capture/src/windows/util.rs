//! Small helpers shared by the Windows code.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// A NUL-terminated UTF-16 copy of `s`, for Windows calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The text in a fixed-size UTF-16 buffer, up to its first NUL.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// The performance counter, the clock every event in a recording is stamped with.
pub fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: writes one i64; cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v as u64
}

pub fn qpc_freq() -> u64 {
    let mut v = 0i64;
    // SAFETY: as above.
    let _ = unsafe { QueryPerformanceFrequency(&mut v) };
    v.max(1) as u64
}

/// A handle closed when dropped.
pub struct Owned(pub HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0 .0.is_null() {
            // SAFETY: the handle is owned by this value and closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

/// Waits up to `limit` for a thread to finish. A thread stuck in a Windows call is left
/// behind rather than hanging the app.
pub fn join_within(handle: JoinHandle<()>, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}
