//! Frames, from Intel's PresentMon run as a separate program. PresentMon reads the graphics
//! events Windows already traces for every program that draws; it doesn't touch the game.

use super::etw::stop_session;
use super::util::{join_within, Owned};
use super::Shared;
use crate::frames::{presentmon_args, FrameFeed, PRESENTMON_SESSION};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MISSING: &str = "Culprit's frame recorder is missing. Put PresentMon (PresentMon-2.6.0-x64.exe from github.com/GameTechDev/PresentMon) next to Culprit.exe.";

/// PresentMon next to Culprit.exe, or wherever CULPRIT_PRESENTMON points.
fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CULPRIT_PRESENTMON").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            name.starts_with("presentmon") && name.ends_with(".exe")
        })
        .collect();
    found.sort();
    found.pop()
}

/// A job that ends PresentMon if Culprit itself goes away.
fn job() -> Result<Owned, String> {
    // SAFETY: the job handle is closed by `Owned`; the limits structure outlives the call.
    unsafe {
        let job = Owned(CreateJobObjectW(None, PCWSTR::null()).map_err(|e| e.message())?);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| e.message())?;
        Ok(job)
    }
}

fn spawn(exe: &Path, job: &Owned) -> Result<Child, String> {
    let child = Command::new(exe)
        .args(presentmon_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Couldn't start PresentMon: {e}"))?;
    // SAFETY: both handles are live; the child's stays open while `child` exists.
    let _ = unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())) };
    Ok(child)
}

/// Keeps the last lines PresentMon printed as errors, so its pipe never fills up.
fn drain_errors(child: &mut Child) -> Arc<Mutex<VecDeque<String>>> {
    let lines = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(stderr) = child.stderr.take() {
        let keep = lines.clone();
        let _ = std::thread::Builder::new().name("culprit presentmon errors".into()).spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut keep = keep.lock().unwrap_or_else(|e| e.into_inner());
                keep.push_back(line);
                if keep.len() > 6 {
                    keep.pop_front();
                }
            }
        });
    }
    lines
}

fn last_error(lines: &Mutex<VecDeque<String>>) -> String {
    let lines = lines.lock().unwrap_or_else(|e| e.into_inner());
    let text: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    text.join(" ")
}

pub struct FrameCapture {
    thread: JoinHandle<()>,
    job: Arc<Owned>,
}

impl FrameCapture {
    pub fn start(shared: Arc<Shared>) -> Result<FrameCapture, String> {
        let exe = find().ok_or(MISSING)?;
        let job = Arc::new(job()?);
        let mut child = spawn(&exe, &job)?;
        let errors = drain_errors(&mut child);
        // When PresentMon can't start its trace it says why and exits straight away.
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(Some(_)) = child.try_wait() {
                std::thread::sleep(Duration::from_millis(100));
                let why = last_error(&errors);
                return Err(if why.is_empty() {
                    "PresentMon stopped right away.".into()
                } else {
                    format!("PresentMon: {why}")
                });
            }
        }
        let thread = std::thread::Builder::new()
            .name("culprit frames".into())
            .spawn({
                let job = job.clone();
                move || read_frames(&shared, &exe, &job, child, errors)
            })
            .map_err(|e| e.to_string())?;
        Ok(FrameCapture { thread, job })
    }

    pub fn stop(self) {
        // Stopping its trace session makes PresentMon finish its output and exit.
        stop_session(PRESENTMON_SESSION);
        join_within(self.thread, Duration::from_secs(4));
        // SAFETY: live job handle; ends PresentMon if it is somehow still running.
        let _ = unsafe { TerminateJobObject(self.job.0, 0) };
    }
}

fn read_frames(shared: &Shared, exe: &Path, job: &Owned, mut child: Child, mut errors: Arc<Mutex<VecDeque<String>>>) {
    let mut restarts = 0;
    loop {
        let mut feed = FrameFeed::new(shared.clock, shared.now());
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::with_capacity(64 * 1024, stdout);
            let mut line = Vec::with_capacity(512);
            loop {
                line.clear();
                match reader.by_ref().take(64 * 1024).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Some((row, t)) = feed.line(&String::from_utf8_lossy(&line)) {
                    shared.with(|s| s.collector.on_frame(&row, t));
                }
            }
        }
        let _ = child.wait();
        if shared.stopping() {
            return;
        }
        if let Some(e) = feed.header_error() {
            shared.note(&format!("Culprit couldn't read PresentMon's output ({e}), so no frames were recorded."));
            return;
        }
        // PresentMon stopped by itself: start it again, a few times at most.
        eprintln!("PresentMon stopped: {}", last_error(&errors));
        restarts += 1;
        if restarts > 3 {
            shared.note("Frame capture stopped partway through this session, so some frames are missing.");
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
        if shared.stopping() {
            return;
        }
        match spawn(exe, job) {
            Ok(mut c) => {
                errors = drain_errors(&mut c);
                child = c;
            }
            Err(_) => {
                shared.note("Frame capture stopped partway through this session, so some frames are missing.");
                return;
            }
        }
    }
}
