//! Records a play session. The modules here turn what Windows reports into a
//! [`Recording`](culpritcore::Recording) and are tested on any platform; `windows` is the
//! code that asks Windows, and [`recorder`] starts and stops a recording on this PC.

pub mod clock;
pub mod collector;
pub mod frames;
pub mod gpu;
pub mod log;
pub mod naming;
pub mod netevents;
pub mod procs;
pub mod recorder;
pub mod targets;
#[cfg(windows)]
mod windows;

pub use recorder::Recorder;
