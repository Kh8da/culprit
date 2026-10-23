//! Starting and stopping a recording on this PC. Only Windows can record; elsewhere every
//! call says so, which keeps the app's code the same on every platform.

use crate::collector::{Live, Options};
use culpritcore::Recording;
use std::time::Duration;

#[cfg(windows)]
use crate::windows as platform;

/// A recording in progress.
pub struct Recorder(platform::Recorder);

impl Recorder {
    /// Starts recording. Needs administrator rights, see [`elevated`].
    pub fn start(opts: Options) -> Result<Self, String> {
        platform::Recorder::start(opts).map(Recorder)
    }

    /// What the live view shows right now.
    pub fn live(&self) -> Live {
        self.0.live()
    }

    /// The player pressed "it lagged" (the app's button; the hotkey is handled inside).
    pub fn mark(&self) {
        self.0.mark()
    }

    /// Stops everything and hands back what was recorded.
    pub fn stop(self) -> Recording {
        self.0.stop()
    }
}

/// Whether this build can record at all.
pub fn supported() -> bool {
    cfg!(windows)
}

/// Whether this process has the rights recording needs: Windows only lets administrators
/// watch frames and network traffic.
pub fn elevated() -> bool {
    platform::elevated()
}

/// Starts this program again as administrator with `args`, after asking the player. Returns
/// once Windows has started it, or an error if the player said no.
pub fn relaunch_elevated(args: &[&str]) -> Result<(), String> {
    platform::relaunch_elevated(args)
}

/// Waits up to `timeout` for process `pid` to exit.
pub fn wait_for_exit(pid: u32, timeout: Duration) {
    platform::wait_for_exit(pid, timeout)
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    const ONLY_WINDOWS: &str = "Culprit can only record on Windows.";

    pub enum Recorder {}

    impl Recorder {
        pub fn start(_: Options) -> Result<Self, String> {
            Err(ONLY_WINDOWS.into())
        }

        pub fn live(&self) -> Live {
            match *self {}
        }

        pub fn mark(&self) {
            match *self {}
        }

        pub fn stop(self) -> Recording {
            match self {}
        }
    }

    pub fn elevated() -> bool {
        false
    }

    pub fn relaunch_elevated(_: &[&str]) -> Result<(), String> {
        Err(ONLY_WINDOWS.into())
    }

    pub fn wait_for_exit(_: u32, _: Duration) {}
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn says_why_it_cannot_record() {
        assert!(!super::supported());
        assert_eq!(super::Recorder::start(Default::default()).err().unwrap(), "Culprit can only record on Windows.");
    }
}
