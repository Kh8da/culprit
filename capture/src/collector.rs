//! Builds a [`Recording`] from the raw events the platform code feeds in: frames from
//! PresentMon, packets from the network trace, pings, and a sample of the whole PC once a
//! second. It also decides which program is the game and which address is its server.
//!
//! Nothing here calls the operating system, so all of it is tested on any platform.

use culpritcore::detect::{is_stutter, spike_threshold};
use culpritcore::model::*;
use culpritcore::names;
use culpritcore::net::{pick_server, Flow, Proto};
use culpritcore::presentmon::PresentRow;
use culpritcore::stats::{percentile, RollingMedian};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

/// Seconds of a game's traffic used to pick its server.
const FLOW_WINDOW: usize = 5;
/// A program has to be in front and drawing this fast for this many seconds to be the game.
const GAME_MIN_FPS: f32 = 20.0;
const GAME_LOCK_SECONDS: u32 = 2;
/// Seconds of frames kept for the live view.
const LIVE_FRAMES_MS: f64 = 10_000.0;
/// Anything stamped later than this is a clock glitch, not a real event.
const MAX_T_MS: f64 = 24.0 * 3_600_000.0;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Lock on to the program with this exe name (case-insensitive) instead of the game in front.
    pub process: Option<String>,
    /// Let a server on the local network or this PC count, for LAN games and tests.
    pub allow_local_server: bool,
}

/// One process as the sampler saw it over the last second.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcReading {
    pub pid: u32,
    pub exe: String,
    /// Grouping key: lower-case exe name, or "svc:<name>" for a Windows service.
    pub key: String,
    pub label: String,
    pub kind: ProcessKind,
    /// Share of the whole CPU, 0 to 100.
    pub cpu: f32,
    /// All I/O the process did, bytes per second. Network I/O is subtracted here.
    pub io: f32,
}

/// What the platform code measured over the last second.
#[derive(Clone, Debug, Default)]
pub struct Tick {
    pub t: f64,
    /// Seconds since the previous tick, for turning byte counts into rates.
    pub dt: f64,
    /// The process owning the window in front, sampled several times during the second.
    pub foreground: Vec<Option<u32>>,
    /// Whole-PC numbers; the game and foreground fields are filled in here.
    pub sample: SystemSample,
    pub procs: Vec<ProcReading>,
}

/// One packet the network trace saw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Packet {
    pub t: f64,
    pub pid: u32,
    pub remote: IpAddr,
    /// The remote port, when the event makes it unambiguous (sends).
    pub port: Option<u16>,
    pub proto: Proto,
    /// True for received packets.
    pub inbound: bool,
    pub bytes: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The recorder locked on to this program as the game. Name it with [`Collector::name_game`].
    GameLocked { pid: u32, exe: String },
    /// The game started talking to a new server; ping it.
    ServerChanged { ip: IpAddr, port: u16 },
}

#[derive(Default)]
struct FlowAgg {
    packets_in: u32,
    packets_out: u32,
    bytes_in: u64,
    bytes_out: u64,
    ports: HashMap<u16, u32>,
}

struct Presenter {
    exe: String,
    times: VecDeque<f64>,
}

#[derive(Default)]
struct LiveTarget {
    last: Option<Option<f32>>,
    baseline: Option<RollingMedian>,
    sent: u32,
    lost: u32,
    /// Worst value per second for the last minute; -1 for a loss.
    history: VecDeque<Option<f32>>,
    second_worst: Option<f32>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LivePing {
    pub id: u32,
    pub role: TargetRole,
    pub ip: String,
    pub last: Option<f32>,
    pub typical: Option<f32>,
    pub loss_pct: f32,
    pub history: Vec<Option<f32>>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveProc {
    pub label: String,
    pub cpu: f32,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Presenting {
    pub pid: u32,
    pub exe: String,
    pub fps: f32,
}

/// What the live view shows while recording.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Live {
    pub elapsed_ms: f64,
    pub game: Option<Game>,
    pub fps: Option<f32>,
    pub low1_fps: Option<f32>,
    /// Recent frame times, oldest first, at most 240.
    pub frames: Vec<f32>,
    pub stutters: u32,
    pub lag_spikes: u32,
    pub pings: Vec<LivePing>,
    pub server: Option<String>,
    pub link: LinkKind,
    pub wifi_signal: Option<f32>,
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub top: Vec<LiveProc>,
    pub marks: u32,
    pub notes: Vec<String>,
    /// Programs drawing frames right now, for picking the game by hand.
    pub presenting: Vec<Presenting>,
}

pub struct Collector {
    opts: Options,
    rec: Recording,
    presenters: HashMap<u32, Presenter>,
    streak: Option<(u32, u32)>,
    game_pid: Option<u32>,
    frame_base: RollingMedian,
    live_stutters: u32,
    recent: VecDeque<Frame>,
    flow_now: HashMap<(IpAddr, Proto), FlowAgg>,
    flow_hist: VecDeque<HashMap<(IpAddr, Proto), FlowAgg>>,
    pid_net: HashMap<u32, (u64, u64)>,
    net_tick: (u64, u64),
    server: Option<IpAddr>,
    server_streak: Option<(IpAddr, u32)>,
    live_lag: u32,
    last_lag: f64,
    live_targets: HashMap<u32, LiveTarget>,
    next_target: u32,
    proc_index: HashMap<String, usize>,
    top: Vec<LiveProc>,
}

impl Collector {
    pub fn new(opts: Options, started_at: u64, machine: Machine, connection: Connection) -> Self {
        Self {
            opts,
            rec: Recording { started_at, machine, connection, ..Default::default() },
            presenters: HashMap::new(),
            streak: None,
            game_pid: None,
            frame_base: RollingMedian::new(90),
            live_stutters: 0,
            recent: VecDeque::new(),
            flow_now: HashMap::new(),
            flow_hist: VecDeque::new(),
            pid_net: HashMap::new(),
            net_tick: (0, 0),
            server: None,
            server_streak: None,
            live_lag: 0,
            last_lag: f64::NEG_INFINITY,
            live_targets: HashMap::new(),
            next_target: 1,
            proc_index: HashMap::new(),
            top: Vec::new(),
        }
    }

    pub fn game_pid(&self) -> Option<u32> {
        self.game_pid
    }

    pub fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !self.rec.notes.contains(&text) {
            self.rec.notes.push(text);
        }
    }

    pub fn set_connection(&mut self, c: Connection) {
        self.rec.connection = c;
    }

    /// A frame PresentMon saw from any program; `t` is on the recording's clock.
    pub fn on_frame(&mut self, row: &PresentRow, t: f64) {
        if !(0.0..MAX_T_MS).contains(&t) || !row.ms_between_presents.is_finite() {
            return;
        }
        let p = self
            .presenters
            .entry(row.pid)
            .or_insert_with(|| Presenter { exe: row.app.clone(), times: VecDeque::new() });
        p.times.push_back(t);
        if Some(row.pid) != self.game_pid {
            return;
        }
        let frame = Frame { t, ms: row.ms_between_presents, cpu_busy: row.cpu_busy, gpu_busy: row.gpu_busy };
        if self.rec.frames.len() >= 60 {
            if let Some(normal) = self.frame_base.median().filter(|_| self.frame_base.len() >= 30) {
                if is_stutter(frame.ms, normal) {
                    // Count a burst of slow frames once.
                    let merged = self.recent.back().is_some_and(|f| is_stutter(f.ms, normal) && t - f.t < 300.0);
                    if !merged {
                        self.live_stutters += 1;
                    }
                } else if frame.ms < 2_000.0 {
                    self.frame_base.push(frame.ms);
                }
            } else {
                self.frame_base.push(frame.ms);
            }
        } else {
            self.frame_base.push(frame.ms);
        }
        self.recent.push_back(frame);
        while self.recent.front().is_some_and(|f| t - f.t > LIVE_FRAMES_MS) {
            self.recent.pop_front();
        }
        self.rec.frames.push(frame);
    }

    /// A packet from the network trace.
    pub fn on_packet(&mut self, pk: Packet) {
        let Packet { t, pid, remote, port, proto, inbound, bytes } = pk;
        let entry = self.pid_net.entry(pid).or_default();
        if inbound {
            entry.0 += bytes as u64;
            self.net_tick.0 += bytes as u64;
        } else {
            entry.1 += bytes as u64;
            self.net_tick.1 += bytes as u64;
        }
        if Some(pid) != self.game_pid || !(0.0..MAX_T_MS).contains(&t) {
            return;
        }
        let bin = (t / 100.0) as usize;
        if bin >= self.rec.game_traffic.len() {
            self.rec.game_traffic.resize(bin + 1, TrafficBin::default());
        }
        let b = &mut self.rec.game_traffic[bin];
        let f = self.flow_now.entry((remote, proto)).or_default();
        if inbound {
            b.packets_in = b.packets_in.saturating_add(1);
            b.bytes_in = b.bytes_in.saturating_add(bytes);
            f.packets_in = f.packets_in.saturating_add(1);
            f.bytes_in += bytes as u64;
        } else {
            b.packets_out = b.packets_out.saturating_add(1);
            b.bytes_out = b.bytes_out.saturating_add(bytes);
            f.packets_out = f.packets_out.saturating_add(1);
            f.bytes_out += bytes as u64;
            if let Some(port) = port {
                *f.ports.entry(port).or_default() += 1;
            }
        }
    }

    pub fn add_target(&mut self, role: TargetRole, ip: IpAddr, host: Option<String>, t: f64) -> u32 {
        let id = self.next_target;
        self.next_target += 1;
        self.rec.targets.push(Target { id, role, ip: ip.to_string(), host, since: t, until: None });
        self.live_targets.insert(id, LiveTarget::default());
        id
    }

    pub fn end_target(&mut self, id: u32, t: f64) {
        if let Some(target) = self.rec.targets.iter_mut().find(|x| x.id == id) {
            target.until.get_or_insert(t);
        }
        self.live_targets.remove(&id);
    }

    pub fn on_ping(&mut self, target: u32, t: f64, rtt: Option<f32>) {
        self.rec.pings.push(Ping { target, t, rtt });
        let role = self.rec.targets.iter().find(|x| x.id == target).map(|x| x.role);
        let Some(lt) = self.live_targets.get_mut(&target) else { return };
        lt.sent += 1;
        lt.last = Some(rtt);
        let base = lt.baseline.get_or_insert_with(|| RollingMedian::new(40));
        let bad = match (rtt, base.median()) {
            (Some(r), Some(n)) if base.len() >= 8 && r > spike_threshold(n) => true,
            (Some(r), _) => {
                base.push(r);
                false
            }
            (None, _) => {
                lt.lost += 1;
                base.len() >= 8
            }
        };
        let v = rtt.unwrap_or(-1.0);
        lt.second_worst = Some(match lt.second_worst {
            Some(w) if w < 0.0 || v < 0.0 => -1.0,
            Some(w) => w.max(v),
            None => v,
        });
        if bad && matches!(role, Some(TargetRole::Server | TargetRole::ServerPath)) && t - self.last_lag > 1_500.0 {
            self.live_lag += 1;
            self.last_lag = t;
        }
    }

    pub fn mark(&mut self, t: f64) {
        self.rec.marks.push(t);
    }

    /// Sets what to call the game once the platform code has looked it up.
    pub fn name_game(&mut self, name: String) {
        if let Some(g) = self.rec.game.as_mut() {
            g.name = name;
        }
    }

    /// Locks on to `pid` as the game, e.g. when the player picks it from the list.
    pub fn lock_game(&mut self, pid: u32, exe: &str, t: f64) -> Event {
        self.game_pid = Some(pid);
        self.rec.game = Some(Game { name: names::exe_stem(exe).to_string(), exe: exe.to_string(), pid, since: t });
        self.frame_base = RollingMedian::new(90);
        self.recent.clear();
        Event::GameLocked { pid, exe: exe.to_string() }
    }

    fn fps(&self, pid: u32, now: f64) -> f32 {
        self.presenters
            .get(&pid)
            .map(|p| p.times.iter().filter(|&&t| now - t <= 2_000.0).count() as f32 / 2.0)
            .unwrap_or(0.0)
    }

    fn detect_game(&mut self, tick: &Tick) -> Option<Event> {
        if self.game_pid.is_some() {
            return None;
        }
        let now = tick.t;
        if let Some(wanted) = self.opts.process.clone() {
            let pid = self
                .presenters
                .iter()
                .filter(|(_, p)| p.exe.eq_ignore_ascii_case(&wanted))
                .map(|(pid, _)| *pid)
                .find(|pid| self.fps(*pid, now) >= 2.0)?;
            let exe = self.presenters[&pid].exe.clone();
            return Some(self.lock_game(pid, &exe, now));
        }
        let fg = tick.foreground.last().copied().flatten();
        let candidate = fg.filter(|pid| {
            self.presenters.get(pid).is_some_and(|p| !names::never_the_game(&p.exe))
                && self.fps(*pid, now) >= GAME_MIN_FPS
        });
        match candidate {
            Some(pid) => {
                let count = match self.streak {
                    Some((p, n)) if p == pid => n + 1,
                    _ => 1,
                };
                self.streak = Some((pid, count));
                if count >= GAME_LOCK_SECONDS {
                    let exe = self.presenters[&pid].exe.clone();
                    return Some(self.lock_game(pid, &exe, now));
                }
            }
            None => self.streak = None,
        }
        None
    }

    fn detect_server(&mut self, t: f64) -> Option<Event> {
        let mut sums: HashMap<(IpAddr, Proto), FlowAgg> = HashMap::new();
        for second in self.flow_hist.iter() {
            for (k, f) in second {
                let s = sums.entry(*k).or_default();
                s.packets_in += f.packets_in;
                s.packets_out += f.packets_out;
                s.bytes_in += f.bytes_in;
                s.bytes_out += f.bytes_out;
                for (port, n) in &f.ports {
                    *s.ports.entry(*port).or_default() += n;
                }
            }
        }
        let flows: Vec<Flow> = sums
            .iter()
            .map(|((ip, proto), f)| Flow {
                ip: *ip,
                port: f.ports.iter().max_by_key(|(_, n)| **n).map(|(p, _)| *p).unwrap_or(0),
                proto: *proto,
                packets_in: f.packets_in,
                packets_out: f.packets_out,
                bytes_in: f.bytes_in,
                bytes_out: f.bytes_out,
            })
            .collect();
        let picked = pick_server(&flows, self.flow_hist.len().max(1) as f64, self.opts.allow_local_server)?;
        if Some(picked.ip) == self.server {
            self.server_streak = None;
            return None;
        }
        // Switch only once the new server has led for two seconds running.
        let n = match self.server_streak {
            Some((ip, n)) if ip == picked.ip => n + 1,
            _ => 1,
        };
        self.server_streak = Some((picked.ip, n));
        if n < 2 {
            return None;
        }
        self.server = Some(picked.ip);
        self.server_streak = None;
        let protocol = if picked.proto == Proto::Udp { "udp" } else { "tcp" };
        self.rec.servers.push(ServerSeen {
            t,
            ip: picked.ip.to_string(),
            port: picked.port,
            protocol: protocol.into(),
        });
        Some(Event::ServerChanged { ip: picked.ip, port: picked.port })
    }

    pub fn on_tick(&mut self, mut tick: Tick) -> Vec<Event> {
        let mut events = Vec::new();
        events.extend(self.detect_game(&tick));
        let t = tick.t;
        let dt = tick.dt.max(0.05);

        let mut s = tick.sample;
        s.t = t;
        s.foreground = self.game_pid.is_some()
            && !tick.foreground.is_empty()
            && tick.foreground.iter().all(|p| *p == self.game_pid);
        s.net_in = (self.net_tick.0 as f64 / dt) as f32;
        s.net_out = (self.net_tick.1 as f64 / dt) as f32;

        // Group everything else by app or service; the game gets its own fields.
        let mut groups: HashMap<String, (String, ProcessKind, ProcSample)> = HashMap::new();
        for r in tick.procs.drain(..) {
            let (net_in, net_out) = self.pid_net.get(&r.pid).copied().unwrap_or((0, 0));
            let (net_in, net_out) = ((net_in as f64 / dt) as f32, (net_out as f64 / dt) as f32);
            let io = (r.io - net_in - net_out).max(0.0);
            if Some(r.pid) == self.game_pid {
                s.game_cpu = Some(s.game_cpu.unwrap_or(0.0) + r.cpu);
                s.game_io = Some(s.game_io.unwrap_or(0.0) + io);
                continue;
            }
            if r.pid == 0 {
                continue;
            }
            let g = groups
                .entry(r.key.clone())
                .or_insert_with(|| (r.label.clone(), r.kind, ProcSample { t, ..Default::default() }));
            g.2.cpu += r.cpu;
            g.2.io += io;
            g.2.net_in += net_in;
            g.2.net_out += net_out;
        }
        let mut top: Vec<LiveProc> = Vec::new();
        for (key, (label, kind, sample)) in groups {
            if sample.cpu >= 2.0 {
                top.push(LiveProc { label: label.clone(), cpu: sample.cpu });
            }
            // Keep only seconds a program was doing something; idle seconds are implied.
            if sample.cpu < 1.0 && sample.io < 1_000_000.0 && sample.net_in < 100_000.0 && sample.net_out < 100_000.0 {
                continue;
            }
            let idx = *self.proc_index.entry(key.clone()).or_insert_with(|| {
                self.rec.processes.push(ProcessTrack { key, label, kind, samples: Vec::new() });
                self.rec.processes.len() - 1
            });
            self.rec.processes[idx].samples.push(sample);
        }
        top.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        top.truncate(3);
        self.top = top;
        self.rec.system.push(s);

        self.pid_net.clear();
        self.net_tick = (0, 0);
        self.flow_hist.push_back(std::mem::take(&mut self.flow_now));
        while self.flow_hist.len() > FLOW_WINDOW {
            self.flow_hist.pop_front();
        }
        if self.game_pid.is_some() {
            events.extend(self.detect_server(t));
        }

        for p in self.presenters.values_mut() {
            while p.times.front().is_some_and(|x| t - x > 2_000.0) {
                p.times.pop_front();
            }
        }
        self.presenters.retain(|_, p| !p.times.is_empty());
        for lt in self.live_targets.values_mut() {
            lt.history.push_back(lt.second_worst.take());
            while lt.history.len() > 60 {
                lt.history.pop_front();
            }
        }
        self.rec.duration_ms = t;
        events
    }

    pub fn live(&self, now: f64) -> Live {
        let frames: Vec<f32> = self.recent.iter().map(|f| f.ms).collect();
        let (fps, low1) = if frames.len() >= 10 {
            let span: f32 = frames.iter().sum();
            let p99 = percentile(&frames, 99.0).unwrap_or(1.0).max(0.001);
            (Some(frames.len() as f32 * 1000.0 / span.max(1.0)), Some(1000.0 / p99))
        } else {
            (None, None)
        };
        let skip = frames.len().saturating_sub(240);
        let mut pings: Vec<LivePing> = self
            .rec
            .targets
            .iter()
            .filter(|t| t.until.is_none())
            .filter_map(|t| {
                let lt = self.live_targets.get(&t.id)?;
                Some(LivePing {
                    id: t.id,
                    role: t.role,
                    ip: t.ip.clone(),
                    last: lt.last.flatten(),
                    typical: lt.baseline.as_ref().and_then(|b| b.median()),
                    loss_pct: if lt.sent == 0 { 0.0 } else { lt.lost as f32 * 100.0 / lt.sent as f32 },
                    history: lt.history.iter().copied().collect(),
                })
            })
            .collect();
        pings.sort_by_key(|p| p.role as u8);
        let last = self.rec.system.last();
        let mut presenting: Vec<Presenting> = self
            .presenters
            .iter()
            .map(|(pid, p)| Presenting { pid: *pid, exe: p.exe.clone(), fps: self.fps(*pid, now) })
            .filter(|p| p.fps >= 1.0)
            .collect();
        presenting.sort_by(|a, b| b.fps.total_cmp(&a.fps));
        Live {
            elapsed_ms: now,
            game: self.rec.game.clone(),
            fps,
            low1_fps: low1,
            frames: frames[skip..].to_vec(),
            stutters: self.live_stutters,
            lag_spikes: self.live_lag,
            pings,
            server: self.server.map(|s| s.to_string()),
            link: self.rec.connection.kind,
            wifi_signal: last.and_then(|s| s.wifi_signal),
            cpu: last.and_then(|s| s.cpu),
            gpu: last.and_then(|s| s.gpu),
            gpu_temp: last.and_then(|s| s.gpu_temp),
            top: self.top.clone(),
            marks: self.rec.marks.len() as u32,
            notes: self.rec.notes.clone(),
            presenting,
        }
    }

    pub fn finish(mut self, t: f64) -> Recording {
        self.rec.duration_ms = t;
        self.rec.frames.sort_by(|a, b| a.t.total_cmp(&b.t));
        self.rec.pings.sort_by(|a, b| a.t.total_cmp(&b.t));
        let bins = (t / 100.0).ceil() as usize;
        self.rec.game_traffic.truncate(bins);
        for target in self.rec.targets.iter_mut() {
            if target.until.is_none() {
                target.until = Some(t);
            }
        }
        self.rec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culpritcore::presentmon::RowTime;

    fn row(pid: u32, app: &str, ms: f32) -> PresentRow {
        PresentRow {
            app: app.into(),
            pid,
            time: RowTime::Ms(0.0),
            ms_between_presents: ms,
            cpu_busy: None,
            gpu_busy: None,
        }
    }

    fn collector() -> Collector {
        Collector::new(Options::default(), 0, Machine::default(), Connection::default())
    }

    fn tick(t: f64, fg: Option<u32>, procs: Vec<ProcReading>) -> Tick {
        Tick {
            t,
            dt: 1.0,
            foreground: vec![fg; 4],
            sample: SystemSample { cpu: Some(30.0), ..Default::default() },
            procs,
        }
    }

    /// Feeds `secs` seconds of frames from each (pid, exe, fps), ticking once a second.
    fn run(c: &mut Collector, from: f64, secs: usize, presenters: &[(u32, &str, f32)], fg: Option<u32>) -> Vec<Event> {
        let mut events = Vec::new();
        for s in 0..secs {
            let start = from + s as f64 * 1000.0;
            for &(pid, exe, fps) in presenters {
                let n = fps as usize;
                for i in 0..n {
                    c.on_frame(&row(pid, exe, 1000.0 / fps), start + i as f64 * 1000.0 / fps as f64);
                }
            }
            events.extend(c.on_tick(tick(start + 1000.0, fg, vec![])));
        }
        events
    }

    #[test]
    fn locks_on_to_the_game_in_front() {
        let mut c = collector();
        // Discord is in front and drawing: never the game.
        let ev = run(&mut c, 0.0, 3, &[(10, "Discord.exe", 60.0), (20, "eldenring.exe", 60.0)], Some(10));
        assert!(ev.is_empty());
        // The game comes to the front; after two seconds it's locked.
        let ev = run(&mut c, 3000.0, 2, &[(10, "Discord.exe", 60.0), (20, "eldenring.exe", 60.0)], Some(20));
        assert_eq!(ev, vec![Event::GameLocked { pid: 20, exe: "eldenring.exe".into() }]);
        assert_eq!(c.game_pid(), Some(20));
        // Only the game's frames are recorded from then on.
        run(&mut c, 5000.0, 2, &[(10, "Discord.exe", 60.0), (20, "eldenring.exe", 60.0)], Some(20));
        let rec = c.finish(7000.0);
        assert_eq!(rec.frames.len(), 120);
        assert_eq!(rec.game.as_ref().unwrap().name, "eldenring");
        assert!(rec.system.last().unwrap().foreground);
        assert!(!rec.system[0].foreground);
    }

    #[test]
    fn slow_drawing_programs_are_not_games() {
        let mut c = collector();
        let ev = run(&mut c, 0.0, 5, &[(30, "notagame.exe", 5.0)], Some(30));
        assert!(ev.is_empty());
    }

    #[test]
    fn named_process_wins() {
        let mut c = Collector::new(
            Options { process: Some("Fake-Game.exe".into()), ..Default::default() },
            0,
            Machine::default(),
            Connection::default(),
        );
        let ev = run(&mut c, 0.0, 1, &[(40, "fake-game.exe", 30.0)], None);
        assert_eq!(ev, vec![Event::GameLocked { pid: 40, exe: "fake-game.exe".into() }]);
    }

    #[test]
    fn finds_the_server_and_counts_traffic() {
        let mut c = collector();
        c.lock_game(7, "cs2.exe", 0.0);
        let server: IpAddr = "155.133.250.9".parse().unwrap();
        let cdn: IpAddr = "23.1.1.1".parse().unwrap();
        let mut events = Vec::new();
        for s in 0..4 {
            for i in 0..64 {
                let t = s as f64 * 1000.0 + i as f64 * 15.0;
                c.on_packet(Packet {
                    t,
                    pid: 7,
                    remote: server,
                    port: Some(27015),
                    proto: Proto::Udp,
                    inbound: false,
                    bytes: 80,
                });
                c.on_packet(Packet {
                    t: t + 5.0,
                    pid: 7,
                    remote: server,
                    port: None,
                    proto: Proto::Udp,
                    inbound: true,
                    bytes: 300,
                });
            }
            for i in 0..400 {
                c.on_packet(Packet {
                    t: s as f64 * 1000.0 + i as f64,
                    pid: 7,
                    remote: cdn,
                    port: None,
                    proto: Proto::Tcp,
                    inbound: true,
                    bytes: 1400,
                });
            }
            // Someone else's traffic doesn't count toward the game.
            c.on_packet(Packet {
                t: s as f64 * 1000.0,
                pid: 99,
                remote: server,
                port: Some(1),
                proto: Proto::Udp,
                inbound: true,
                bytes: 50_000,
            });
            events.extend(c.on_tick(tick(s as f64 * 1000.0 + 1000.0, Some(7), vec![])));
        }
        assert_eq!(events, vec![Event::ServerChanged { ip: server, port: 27015 }]);
        let rec = c.finish(4000.0);
        assert_eq!(rec.servers.len(), 1);
        assert_eq!(rec.servers[0].port, 27015);
        assert_eq!(rec.game_traffic.len(), 40);
        assert!(rec.game_traffic[0].packets_in >= 6);
        // 64 * 300 + 400 * 1400 + 50_000 bytes in during the first second.
        assert_eq!(rec.system[0].net_in, (64 * 300 + 400 * 1400 + 50_000) as f32);
    }

    #[test]
    fn groups_processes_and_splits_network_from_disk() {
        let mut c = collector();
        c.lock_game(7, "cs2.exe", 0.0);
        let r = |pid: u32, exe: &str, cpu: f32, io: f32| ProcReading {
            pid,
            exe: exe.into(),
            key: exe.to_lowercase(),
            label: names::app_label(exe).unwrap_or(exe).into(),
            kind: ProcessKind::App,
            cpu,
            io,
        };
        c.on_packet(Packet {
            t: 10.0,
            pid: 300,
            remote: "1.1.1.1".parse().unwrap(),
            port: None,
            proto: Proto::Tcp,
            inbound: true,
            bytes: 5_000_000,
        });
        let procs = vec![
            r(7, "cs2.exe", 40.0, 2_000_000.0),
            r(100, "chrome.exe", 3.0, 0.0),
            r(101, "chrome.exe", 4.0, 0.0),
            r(200, "notepad.exe", 0.0, 0.0),
            r(300, "steam.exe", 1.0, 5_500_000.0),
        ];
        c.on_tick(tick(1000.0, Some(7), procs));
        let live = c.live(1000.0);
        assert_eq!(live.top[0], LiveProc { label: "Google Chrome".into(), cpu: 7.0 });
        let rec = c.finish(1000.0);
        let s = rec.system[0];
        assert_eq!(s.game_cpu, Some(40.0));
        assert_eq!(s.game_io, Some(2_000_000.0));
        assert!(s.foreground);
        let chrome = rec.processes.iter().find(|p| p.key == "chrome.exe").unwrap();
        assert_eq!(chrome.samples[0].cpu, 7.0);
        let steam = rec.processes.iter().find(|p| p.key == "steam.exe").unwrap();
        assert_eq!(steam.samples[0].net_in, 5_000_000.0);
        assert_eq!(steam.samples[0].io, 500_000.0);
        assert!(rec.processes.iter().all(|p| p.key != "notepad.exe"));
    }

    #[test]
    fn ignores_glitched_timestamps() {
        let mut c = collector();
        c.lock_game(7, "game.exe", 0.0);
        c.on_packet(Packet {
            t: 1e15,
            pid: 7,
            remote: "1.2.3.4".parse().unwrap(),
            port: Some(1),
            proto: Proto::Udp,
            inbound: true,
            bytes: 100,
        });
        c.on_frame(&row(7, "game.exe", 7.0), -5.0);
        c.on_frame(&row(7, "game.exe", 7.0), 1e15);
        let rec = c.finish(1000.0);
        assert!(rec.game_traffic.is_empty());
        assert!(rec.frames.is_empty());
    }

    #[test]
    fn live_view_counts_stutters_and_lag() {
        let mut c = collector();
        c.lock_game(7, "game.exe", 0.0);
        for i in 0..600 {
            let ms = if i == 300 || i == 301 {
                80.0
            } else if i == 500 {
                120.0
            } else {
                7.0
            };
            c.on_frame(&row(7, "game.exe", ms), i as f64 * 7.0);
        }
        let server = c.add_target(TargetRole::Server, "155.133.250.9".parse().unwrap(), None, 0.0);
        for i in 0..40 {
            let rtt = if i == 30 { Some(400.0) } else { Some(30.0) };
            c.on_ping(server, i as f64 * 250.0, rtt);
        }
        c.on_tick(tick(10_000.0, Some(7), vec![]));
        let live = c.live(10_000.0);
        assert_eq!(live.stutters, 2);
        assert_eq!(live.lag_spikes, 1);
        assert_eq!(live.pings[0].last, Some(30.0));
        assert_eq!(live.pings[0].history, vec![Some(400.0)]);
        assert!(live.fps.unwrap() > 50.0);
    }
}
