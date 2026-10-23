//! Running PresentMon and reading its output: the command line, and turning each CSV line
//! into a frame on the recording's clock.

use crate::clock::Clock;
use culpritcore::presentmon::{CsvLayout, PresentRow, RowTime};

/// Name of PresentMon's trace session, so a session left over from a crash can be stopped.
pub const PRESENTMON_SESSION: &str = "CulpritFrames";

/// PresentMon's command line. It prints one CSV row per frame from every program that
/// draws, stamped with the performance counter. Input tracking is off because Culprit
/// doesn't use it, and the desktop compositor is left out because it is never the game.
pub fn presentmon_args() -> Vec<&'static str> {
    vec![
        "--output_stdout",
        "--no_console_stats",
        "--qpc_time",
        "--no_track_input",
        "--exclude",
        "dwm.exe",
        "--session_name",
        PRESENTMON_SESSION,
        "--stop_existing_session",
    ]
}

/// Reads PresentMon's output one line at a time.
pub struct FrameFeed {
    layout: Option<CsvLayout>,
    clock: Clock,
    /// When PresentMon started on the recording's clock, for rows stamped relative to it.
    started_ms: f64,
    header_error: Option<String>,
}

impl FrameFeed {
    pub fn new(clock: Clock, started_ms: f64) -> Self {
        Self { layout: None, clock, started_ms, header_error: None }
    }

    /// The frame on this line and its time, or `None` for the header and anything else.
    pub fn line(&mut self, line: &str) -> Option<(PresentRow, f64)> {
        let Some(layout) = &self.layout else {
            // Warnings can come first; the header is the first line that has the columns.
            match CsvLayout::from_header(line) {
                Ok(l) => self.layout = Some(l),
                Err(e) if line.starts_with("Application") => self.header_error = Some(e),
                Err(_) => {}
            }
            return None;
        };
        let row = layout.parse(line)?;
        let t = match row.time {
            RowTime::Qpc(v) => self.clock.ms(v),
            RowTime::Ms(ms) => self.started_ms + ms,
        };
        Some((row, t))
    }

    /// Why the output couldn't be read, if the header was missing columns.
    pub fn header_error(&self) -> Option<&str> {
        self.header_error.as_deref().filter(|_| self.layout.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,AllowsTearing,PresentMode,TimeInQPC,MsBetweenSimulationStart,MsBetweenPresents,MsBetweenDisplayChange,MsInPresentAPI,MsRenderPresentLatency,MsUntilDisplayed,CPUStartQPC,MsBetweenAppStart,MsCPUBusy,MsCPUWait,MsGPULatency,MsGPUTime,MsGPUBusy,MsGPUWait,MsAnimationError,AnimationTime,MsFlipDelay";

    #[test]
    fn rows_land_on_the_recording_clock() {
        let clock = Clock { origin: 1_000_000, freq: 10_000_000 };
        let mut feed = FrameFeed::new(clock, 0.0);
        assert!(feed.line("warning: a trace session named \"CulpritFrames\" is already running").is_none());
        assert!(feed.line(HEADER).is_none());
        let (row, t) = feed
            .line("cs2.exe,4120,0x1,DXGI,0,512,1,Hardware: Independent Flip,1100000,NA,6.94,6.94,0.1,4.2,5.5,1099000,6.95,5.1,1.8,0.5,4.0,3.9,0.1,NA,NA,0.0")
            .unwrap();
        assert_eq!(row.pid, 4120);
        assert_eq!(t, 10.0);
        assert_eq!(row.gpu_busy, Some(3.9));
        assert!(feed.line("Stopped recording.").is_none());
        assert!(feed.header_error().is_none());
    }

    #[test]
    fn reports_an_unusable_header() {
        let mut feed = FrameFeed::new(Clock { origin: 0, freq: 1 }, 0.0);
        feed.line("Application,ProcessID");
        assert!(feed.header_error().unwrap().contains("frame time"));
    }

    #[test]
    fn command_line_asks_for_counter_timestamps() {
        let args = presentmon_args();
        assert!(args.contains(&"--qpc_time") && args.contains(&"--output_stdout"));
        let i = args.iter().position(|a| *a == "--session_name").unwrap();
        assert_eq!(args[i + 1], PRESENTMON_SESSION);
    }
}
