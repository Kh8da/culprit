//! A plain text log of what the app and the recorder did, so a problem on a PC nobody else
//! can see can still be worked out afterwards. One file, appended to, kept to a few MB.
//!
//! Write to it with [`log!`](crate::log!), like `format!`.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A log bigger than this is set aside (as culprit.old.log) when the app starts.
const KEEP_BYTES: u64 = 4 * 1024 * 1024;
/// More than this in one run means something is repeating itself; stop writing.
const RUN_BYTES: u64 = 8 * 1024 * 1024;

struct Log {
    file: File,
    written: u64,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Starts writing to `dir/culprit.log`. Returns the file's path.
pub fn open(dir: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join("culprit.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > KEEP_BYTES) {
        let _ = fs::rename(&path, dir.join("culprit.old.log"));
    }
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(Log { file, written: 0 });
    Ok(path)
}

/// Writes one line, stamped with the time. Before [`open`] it only goes to the console.
pub fn write(args: fmt::Arguments) {
    let line = format!("{} {args}\n", timestamp(SystemTime::now()));
    if cfg!(debug_assertions) {
        eprint!("{line}");
    }
    let mut guard = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(log) = guard.as_mut() {
        if log.written > RUN_BYTES {
            return;
        }
        log.written += line.len() as u64;
        let _ = log.file.write_all(line.as_bytes());
        if log.written > RUN_BYTES {
            let _ = log.file.write_all(b"(log full, nothing more is written this run)\n");
        }
    }
}

/// Writes a line to the log, like `format!`.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write(format_args!($($arg)*))
    };
}

/// "2026-10-23 14:05:03.123Z", in UTC.
fn timestamp(t: SystemTime) -> String {
    let ms = t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let (days, ms) = (ms / 86_400_000, ms % 86_400_000);
    let (y, m, d) = civil_date(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// Year, month and day of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Bytes as hex, for showing raw data the recorder couldn't make sense of.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_in_utc() {
        let at = |ms: u64| timestamp(UNIX_EPOCH + Duration::from_millis(ms));
        assert_eq!(at(0), "1970-01-01 00:00:00.000Z");
        assert_eq!(at(951_782_400_000), "2000-02-29 00:00:00.000Z");
        assert_eq!(at(1_792_727_103_456), "2026-10-23 03:45:03.456Z");
        assert_eq!(hex(&[0, 0xab, 7]), "00 ab 07");
    }

    #[test]
    fn writes_and_sets_old_logs_aside() {
        let dir = std::env::temp_dir().join(format!("culprit-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("culprit.log"), vec![b'x'; KEEP_BYTES as usize + 1]).unwrap();
        let path = open(&dir).unwrap();
        crate::log!("recording started, {} targets", 3);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("Z recording started, 3 targets\n"), "{text}");
        assert!(dir.join("culprit.old.log").exists());
        *LOG.lock().unwrap() = None;
        let _ = fs::remove_dir_all(&dir);
    }
}
