//! Everything a recording holds. Times are milliseconds since the recording started,
//! and a 1-second sample stamped `t` covers the second that ended at `t`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    /// Wall-clock start, in milliseconds since the Unix epoch.
    pub started_at: u64,
    pub duration_ms: f64,
    pub machine: Machine,
    pub game: Option<Game>,
    /// Frames the game presented, oldest first.
    pub frames: Vec<Frame>,
    pub targets: Vec<Target>,
    pub pings: Vec<Ping>,
    /// The game's own traffic in 100 ms bins: bin `i` covers `[i * 100, i * 100 + 100)`.
    pub game_traffic: Vec<TrafficBin>,
    /// Servers the game talked to, in the order it switched to them.
    pub servers: Vec<ServerSeen>,
    pub system: Vec<SystemSample>,
    /// Everything else running, grouped by app or service, kept only for seconds it was busy.
    pub processes: Vec<ProcessTrack>,
    pub connection: Connection,
    /// Moments the player pressed the "it lagged" hotkey.
    pub marks: Vec<f64>,
    /// Things the recorder couldn't measure on this PC, in plain words.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub cpu: Option<String>,
    pub logical_cpus: u32,
    pub mem_total_mb: Option<f32>,
    pub gpu: Option<String>,
    pub vram_total_mb: Option<f32>,
    pub os: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    /// What the player would call it, e.g. "Counter-Strike 2".
    pub name: String,
    /// The executable's file name, e.g. "cs2.exe".
    pub exe: String,
    pub pid: u32,
    /// When the recorder locked on to it.
    pub since: f64,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    /// When the game handed the frame to Windows.
    pub t: f64,
    /// Time since the previous frame.
    pub ms: f32,
    /// How long the CPU worked on this frame, when PresentMon could tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy: Option<f32>,
    /// How long the GPU worked on this frame, when PresentMon could tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_busy: Option<f32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum TargetRole {
    /// The first hop: your router or Wi-Fi access point.
    Router,
    /// The first hop that belongs to your internet provider.
    Isp,
    /// The game server itself.
    Server,
    /// The last hop toward a game server that doesn't answer pings.
    ServerPath,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: u32,
    pub role: TargetRole,
    pub ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub since: f64,
    /// Set when the recorder stopped pinging it, e.g. after the game changed servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ping {
    pub target: u32,
    /// When the ping was sent.
    pub t: f64,
    /// Round trip in milliseconds, or `None` if no answer came back within a second.
    pub rtt: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficBin {
    pub packets_in: u32,
    pub packets_out: u32,
    pub bytes_in: u32,
    pub bytes_out: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerSeen {
    pub t: f64,
    pub ip: String,
    pub port: u16,
    /// "udp" or "tcp".
    pub protocol: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GpuThrottle {
    /// The GPU slowed itself down because it got too hot.
    pub thermal: bool,
    /// The board or power supply forced a slowdown.
    pub power_brake: bool,
    /// A hardware slowdown signal (heat or power) was active.
    pub hw_slowdown: bool,
}

impl GpuThrottle {
    pub fn any(&self) -> bool {
        self.thermal || self.power_brake || self.hw_slowdown
    }
}

/// One second of whole-PC measurements. Each field is `None` when this PC couldn't measure it.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SystemSample {
    pub t: f64,
    /// CPU use across all cores, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f32>,
    /// CPU clock speed as a percentage of its rated speed. Boosting goes above 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_perf: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_free_mb: Option<f32>,
    /// Pages Windows had to read back from disk because RAM ran short, per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_faults: Option<f32>,
    /// How busy the disks were, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_busy: Option<f32>,
    /// GPU 3D engine use, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_used_mb: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_temp: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_clock: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_throttle: Option<GpuThrottle>,
    /// The game's own CPU use, 0 to 100 of the whole CPU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_cpu: Option<f32>,
    /// The game's own disk and file reads and writes, bytes per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_io: Option<f32>,
    /// Whether the game was the window in front for this whole second.
    pub foreground: bool,
    /// Wi-Fi signal quality, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_signal: Option<f32>,
    /// All network traffic on the PC, bytes per second.
    pub net_in: f32,
    pub net_out: f32,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProcessKind {
    #[default]
    App,
    /// A Windows service, e.g. Windows Update running inside svchost.exe.
    Service,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessTrack {
    /// Stable grouping key: the lower-case exe name, or "svc:<name>" for a service.
    pub key: String,
    /// What to show a person, e.g. "Microsoft Defender".
    pub label: String,
    pub kind: ProcessKind,
    pub samples: Vec<ProcSample>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcSample {
    pub t: f64,
    /// Share of the whole CPU, 0 to 100.
    pub cpu: f32,
    /// Disk and file I/O, bytes per second.
    pub io: f32,
    /// Network traffic, bytes per second.
    pub net_in: f32,
    pub net_out: f32,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LinkKind {
    #[default]
    Unknown,
    Ethernet,
    Wifi,
    Other,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub kind: LinkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// 2.4, 5 or 6, when on Wi-Fi.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_band_ghz: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_mbps: Option<f32>,
}
