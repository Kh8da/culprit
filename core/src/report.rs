//! What Culprit tells the player after a session.

use crate::model::LinkKind;
use serde::{Deserialize, Serialize};

pub const REPORT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub version: u32,
    /// One sentence verdict, e.g. "Microsoft Defender caused 14 of your 20 stutters."
    pub headline: String,
    /// A sentence or two backing it up.
    pub detail: String,
    pub mood: Mood,
    pub game: Option<String>,
    pub duration_ms: f64,
    pub perf: Option<PerfSummary>,
    pub net: Option<NetSummary>,
    /// Causes, most damaging first.
    pub culprits: Vec<Culprit>,
    /// Every stutter and lag spike, oldest first.
    pub incidents: Vec<Incident>,
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Mood {
    Good,
    Fair,
    Bad,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerfSummary {
    pub frames: u32,
    pub avg_fps: f32,
    /// Frame rate of the slowest 1% of frames.
    pub low1_fps: f32,
    pub stutters: u32,
    pub stutters_per_min: f32,
    /// The longest single frame, in milliseconds.
    pub worst_ms: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetSummary {
    pub link: LinkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_signal: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_band_ghz: Option<f32>,
    /// The game server's address, if the game was talking to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// False when the server ignores pings, so its ping comes from the last hop before it.
    pub server_answers_ping: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ping_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isp_ms: Option<f32>,
    pub lag_spikes: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum IncidentKind {
    /// The game's frames hitched.
    Stutter,
    /// The connection to the game slowed down or dropped packets.
    Lag,
    /// The player pressed the hotkey and Culprit found nothing worse nearby.
    Marked,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Cause {
    /// Another program used a burst of CPU.
    App { key: String, label: String },
    /// Another program hammered the disk.
    AppDisk { key: String, label: String },
    /// Another program used the internet connection heavily.
    AppNetwork { key: String, label: String },
    CpuMaxed,
    CpuSlowed,
    RamFull,
    VramFull,
    GpuThrottled,
    /// The game's own CPU work, often shader compiling or streaming in a new area.
    GameCpu,
    /// The game's own GPU work.
    GameGpu,
    /// The game reading from disk.
    GameLoading,
    /// Nothing else was going on, so the game itself.
    Game,
    /// Between the PC and the router.
    HomeNetwork { wifi: bool },
    /// The internet provider.
    Isp,
    /// The game server or the route to it.
    Server,
    Unknown,
}

impl Cause {
    /// Short name for lists, e.g. "Microsoft Defender" or "Your Wi-Fi".
    pub fn title(&self) -> String {
        match self {
            Cause::App { label, .. } | Cause::AppDisk { label, .. } | Cause::AppNetwork { label, .. } => label.clone(),
            Cause::CpuMaxed => "CPU maxed out".into(),
            Cause::CpuSlowed => "CPU slowed down".into(),
            Cause::RamFull => "Out of RAM".into(),
            Cause::VramFull => "GPU memory full".into(),
            Cause::GpuThrottled => "GPU slowed down".into(),
            Cause::GameCpu => "The game (CPU work)".into(),
            Cause::GameGpu => "The game (GPU work)".into(),
            Cause::GameLoading => "The game (loading)".into(),
            Cause::Game => "The game itself".into(),
            Cause::HomeNetwork { wifi: true } => "Your Wi-Fi".into(),
            Cause::HomeNetwork { wifi: false } => "Your home network".into(),
            Cause::Isp => "Your internet provider".into(),
            Cause::Server => "The game server".into(),
            Cause::Unknown => "Unclear".into(),
        }
    }

    pub fn is_game(&self) -> bool {
        matches!(self, Cause::GameCpu | Cause::GameGpu | Cause::GameLoading | Cause::Game)
    }

    /// Whether this cause is something on the player's side they can act on.
    pub fn is_actionable(&self) -> bool {
        !matches!(self, Cause::Unknown | Cause::Server) && !self.is_game()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Culprit {
    pub cause: Cause,
    pub title: String,
    pub kind: IncidentKind,
    /// How many incidents of `kind` this cause got.
    pub count: u32,
    /// How many incidents of `kind` there were in total.
    pub of: u32,
    pub confidence: Confidence,
    /// The evidence across the session, in a sentence or two.
    pub why: String,
    /// What to do about it.
    pub fix: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub t: f64,
    pub dur: f64,
    pub kind: IncidentKind,
    /// What happened, e.g. "Frame took 182 ms" or "Ping jumped to 240 ms".
    pub what: String,
    /// The worst frame time or ping, for sorting and drawing.
    pub size_ms: f32,
    pub cause: Cause,
    pub cause_title: String,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
    /// The player pressed the hotkey within a few seconds after it.
    pub marked: bool,
}
