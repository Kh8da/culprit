//! Culprit's brain: what a recorded play session contains, and how to turn it into a verdict
//! about what made the game lag or stutter. Nothing here touches the operating system, so it
//! runs and tests the same everywhere.

pub mod analyze;
pub mod attribute;
pub mod charts;
pub mod demo;
pub mod detect;
pub mod model;
pub mod names;
pub mod net;
pub mod presentmon;
pub mod report;
pub mod stats;

pub use analyze::analyze;
pub use model::Recording;
pub use report::Report;

use serde::{Deserialize, Serialize};

pub const SESSION_VERSION: u32 = 1;

/// What gets saved after a session: the verdict and enough to draw it again later.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub started_at: u64,
    pub machine: model::Machine,
    pub connection: model::Connection,
    pub report: Report,
    pub charts: charts::Charts,
}

pub fn summarize(rec: &Recording) -> Session {
    Session {
        version: SESSION_VERSION,
        id: rec.started_at.to_string(),
        started_at: rec.started_at,
        machine: rec.machine.clone(),
        connection: rec.connection.clone(),
        report: analyze(rec),
        charts: charts::build(rec),
    }
}
