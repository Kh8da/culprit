//! Finding the moments that went wrong: frame hitches and connection hiccups.

use crate::model::{Ping, Recording, SystemSample, TargetRole};
use crate::stats::{percentile, RollingMedian};

/// A frame longer than this is a pause (loading screen, alt-tab, breakpoint), not a stutter.
const PAUSE_MS: f32 = 2_000.0;
/// Frames this close together belong to the same hitch.
const STUTTER_MERGE_MS: f64 = 300.0;
/// Frames ignored at the start while the baseline settles.
const WARMUP_FRAMES: usize = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct Stutter {
    pub t: f64,
    pub end: f64,
    pub worst_ms: f32,
    /// The frame time the game was running at just before.
    pub normal_ms: f32,
    pub frames: u32,
    pub cpu_busy: Option<f32>,
    pub gpu_busy: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub frames: u32,
    pub avg_fps: f32,
    pub low1_fps: f32,
    pub worst_ms: f32,
    /// Minutes of play the stats cover: foreground time with frames.
    pub minutes: f32,
}

/// Whether the game was in front at time `t`. With no system samples, assume it was.
pub fn foreground_at(system: &[SystemSample], t: f64) -> bool {
    if system.is_empty() {
        return true;
    }
    // A sample stamped `t` covers the second before it, so the first sample at or after `t`.
    let i = system.partition_point(|s| s.t < t);
    system.get(i).or(system.last()).map(|s| s.foreground).unwrap_or(true)
}

/// A frame is a stutter when it takes much longer than the frames just before it and the
/// difference is big enough to feel: at least 2.5 times as long, 12 ms longer, and 25 ms.
pub fn is_stutter(ms: f32, normal: f32) -> bool {
    ms >= normal * 2.5 && ms - normal >= 12.0 && ms >= 25.0 && ms < PAUSE_MS
}

pub fn find_stutters(rec: &Recording) -> (Vec<Stutter>, FrameStats) {
    let mut baseline = RollingMedian::new(90);
    let mut out: Vec<Stutter> = Vec::new();
    let mut counted: Vec<f32> = Vec::new();
    let mut played_ms = 0.0f64;
    for (i, f) in rec.frames.iter().enumerate() {
        if f.ms >= PAUSE_MS || !foreground_at(&rec.system, f.t) {
            continue;
        }
        let normal = baseline.median();
        if i >= WARMUP_FRAMES {
            counted.push(f.ms);
            played_ms += f.ms as f64;
            if let Some(normal) = normal.filter(|_| baseline.len() >= 30) {
                if is_stutter(f.ms, normal) {
                    let start = f.t - f.ms as f64;
                    match out.last_mut() {
                        Some(s) if start - s.end <= STUTTER_MERGE_MS => {
                            s.end = f.t;
                            s.frames += 1;
                            if f.ms > s.worst_ms {
                                s.worst_ms = f.ms;
                                s.cpu_busy = f.cpu_busy;
                                s.gpu_busy = f.gpu_busy;
                            }
                        }
                        _ => out.push(Stutter {
                            t: start,
                            end: f.t,
                            worst_ms: f.ms,
                            normal_ms: normal,
                            frames: 1,
                            cpu_busy: f.cpu_busy,
                            gpu_busy: f.gpu_busy,
                        }),
                    }
                    // Hitches stay out of the baseline so a burst of them doesn't hide the rest.
                    continue;
                }
            }
        }
        baseline.push(f.ms);
    }
    let stats = if counted.is_empty() || played_ms <= 0.0 {
        FrameStats::default()
    } else {
        let p99 = percentile(&counted, 99.0).unwrap_or(0.0).max(0.001);
        FrameStats {
            frames: counted.len() as u32,
            avg_fps: (counted.len() as f64 / (played_ms / 1000.0)) as f32,
            low1_fps: 1000.0 / p99,
            worst_ms: counted.iter().copied().fold(0.0, f32::max),
            minutes: (played_ms / 60_000.0) as f32,
        }
    };
    (out, stats)
}

#[derive(Clone, Debug, PartialEq)]
pub enum LagSign {
    /// The game path answered slowly: worst round trip vs. normal.
    Spike { rtt: f32, normal: f32 },
    /// Pings on the game path went unanswered.
    Loss { lost: u32 },
    /// The game stopped receiving anything for this long.
    Silence { ms: f32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lag {
    pub t: f64,
    pub end: f64,
    pub signs: Vec<LagSign>,
}

impl Lag {
    /// The number to show: worst ping, or silence length.
    pub fn size_ms(&self) -> f32 {
        self.signs
            .iter()
            .map(|s| match s {
                LagSign::Spike { rtt, .. } => *rtt,
                LagSign::Silence { ms } => *ms,
                LagSign::Loss { .. } => 0.0,
            })
            .fold(0.0, f32::max)
    }
}

/// A target's ping history split into what's normal and what isn't.
pub struct PingTrack {
    pub target: u32,
    /// Answered pings out of all sent.
    pub answer_rate: f32,
    /// The typical round trip.
    pub normal: Option<f32>,
    /// Pings that were spikes or losses, with the normal value at that moment.
    pub bad: Vec<(Ping, f32)>,
}

impl PingTrack {
    /// Whether the target answers reliably enough for its silence to mean something.
    pub fn trustworthy(&self) -> bool {
        self.answer_rate >= 0.8 && self.normal.is_some()
    }

    pub fn bad_between(&self, a: f64, b: f64) -> impl Iterator<Item = &(Ping, f32)> {
        self.bad.iter().filter(move |(p, _)| p.t >= a && p.t <= b)
    }
}

/// How far above normal a round trip must be to count as a spike.
pub fn spike_threshold(normal: f32) -> f32 {
    normal + (normal * 0.8).max(25.0)
}

pub fn track_pings(rec: &Recording, target: u32) -> PingTrack {
    let pings: Vec<Ping> = rec.pings.iter().filter(|p| p.target == target).copied().collect();
    let answered = pings.iter().filter(|p| p.rtt.is_some()).count();
    let answer_rate = if pings.is_empty() { 0.0 } else { answered as f32 / pings.len() as f32 };
    let all: Vec<f32> = pings.iter().filter_map(|p| p.rtt).collect();
    let normal = crate::stats::median(&all);
    let mut rolling = RollingMedian::new(40);
    let mut bad = Vec::new();
    for p in &pings {
        let local = if rolling.len() >= 8 { rolling.median() } else { None };
        let reference = local.or(normal);
        match (p.rtt, reference) {
            (Some(rtt), Some(n)) if rtt > spike_threshold(n) => bad.push((*p, n)),
            (Some(rtt), _) => rolling.push(rtt),
            (None, Some(n)) if answered > 0 => bad.push((*p, n)),
            _ => {}
        }
    }
    PingTrack { target, answer_rate, normal, bad }
}

/// The targets that stand for "the way to the game": the server when it answers pings,
/// otherwise the last hop before it.
pub fn path_targets(rec: &Recording) -> Vec<u32> {
    let servers: Vec<u32> = rec
        .targets
        .iter()
        .filter(|t| t.role == TargetRole::Server)
        .map(|t| t.id)
        .filter(|&id| track_pings(rec, id).trustworthy())
        .collect();
    if !servers.is_empty() {
        return servers;
    }
    rec.targets
        .iter()
        .filter(|t| t.role == TargetRole::ServerPath)
        .map(|t| t.id)
        .filter(|&id| track_pings(rec, id).trustworthy())
        .collect()
}

pub fn find_lag(rec: &Recording) -> Vec<Lag> {
    let mut events: Vec<Lag> = Vec::new();

    for id in path_targets(rec) {
        let track = track_pings(rec, id);
        // One stray slow or lost ping is noise (routers deprioritise pings); two within
        // two seconds, or one very slow one, is lag.
        let bad = &track.bad;
        for (p, normal) in bad {
            let neighbours = bad.iter().filter(|(q, _)| (q.t - p.t).abs() <= 2_000.0).count();
            let big = p.rtt.is_some_and(|r| r > normal + normal.max(40.0));
            if neighbours < 2 && !big {
                continue;
            }
            let sign = match p.rtt {
                Some(rtt) => LagSign::Spike { rtt, normal: *normal },
                None => LagSign::Loss { lost: 1 },
            };
            let end = p.t + p.rtt.map(f64::from).unwrap_or(1_000.0);
            events.push(Lag { t: p.t, end, signs: vec![sign] });
        }
    }

    // Silences: the game normally gets a steady stream from the server, then nothing.
    let bins = &rec.game_traffic;
    let mut rolling = RollingMedian::new(50);
    let mut i = 0;
    while i < bins.len() {
        let normal = rolling.median().unwrap_or(0.0);
        if bins[i].packets_in == 0 && rolling.len() >= 30 && normal >= 2.0 {
            let start = i;
            while i < bins.len() && bins[i].packets_in == 0 {
                i += 1;
            }
            let ms = ((i - start) * 100) as f32;
            // Short gaps are normal jitter; very long ones are a map change or disconnect.
            if (300.0..=5_000.0).contains(&ms) && i < bins.len() {
                let t = start as f64 * 100.0;
                events.push(Lag { t, end: t + ms as f64, signs: vec![LagSign::Silence { ms }] });
            }
            continue;
        }
        rolling.push(bins[i].packets_in as f32);
        i += 1;
    }

    events.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut merged: Vec<Lag> = Vec::new();
    for e in events {
        match merged.last_mut() {
            Some(m) if e.t - m.end <= 1_500.0 => {
                m.end = m.end.max(e.end);
                for s in e.signs {
                    match (&s, m.signs.iter_mut().find(|x| std::mem::discriminant(*x) == std::mem::discriminant(&s))) {
                        (LagSign::Loss { lost }, Some(LagSign::Loss { lost: total })) => *total += lost,
                        (LagSign::Spike { rtt, .. }, Some(LagSign::Spike { rtt: worst, .. })) => *worst = worst.max(*rtt),
                        (LagSign::Silence { ms }, Some(LagSign::Silence { ms: longest })) => *longest = longest.max(*ms),
                        _ => m.signs.push(s),
                    }
                }
            }
            _ => merged.push(e),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, Target, TrafficBin};

    fn frames(ms: impl Fn(usize) -> f32, n: usize) -> Vec<Frame> {
        let mut t = 0.0;
        (0..n)
            .map(|i| {
                let ms = ms(i);
                t += ms as f64;
                Frame { t, ms, cpu_busy: None, gpu_busy: None }
            })
            .collect()
    }

    #[test]
    fn finds_hitches_not_noise() {
        // 144 fps with jitter, three hitches, one of them two frames long, and a loading pause.
        let mut rec = Recording::default();
        rec.frames = frames(
            |i| match i {
                500 => 60.0,
                1200 | 1201 => 45.0,
                2000 => 3_000.0,
                2600 => 90.0,
                _ => 6.9 + (i % 5) as f32 * 0.4,
            },
            3000,
        );
        let (stutters, stats) = find_stutters(&rec);
        assert_eq!(stutters.len(), 3, "{stutters:?}");
        assert_eq!(stutters[1].frames, 2);
        assert_eq!(stutters[2].worst_ms, 90.0);
        assert!((stats.avg_fps - 130.0).abs() < 15.0, "{stats:?}");
        assert_eq!(stats.worst_ms, 90.0);
    }

    #[test]
    fn a_slow_steady_game_is_not_stuttering() {
        let mut rec = Recording::default();
        rec.frames = frames(|i| if i % 2 == 0 { 30.0 } else { 36.0 }, 1000);
        assert!(find_stutters(&rec).0.is_empty());
    }

    #[test]
    fn background_time_is_ignored() {
        let mut rec = Recording::default();
        rec.frames = frames(|i| if i == 400 { 100.0 } else { 16.7 }, 600);
        let t = rec.frames[400].t;
        rec.system = (1..=11).map(|s| SystemSample { t: s as f64 * 1000.0, foreground: (s as f64 * 1000.0) < t || (s as f64 * 1000.0) > t + 1000.0, ..Default::default() }).collect();
        assert!(find_stutters(&rec).0.is_empty());
    }

    fn pinged(rtts: &[Option<f32>]) -> Recording {
        let mut rec = Recording::default();
        rec.targets.push(Target { id: 1, role: TargetRole::Server, ip: "1.2.3.4".into(), host: None, since: 0.0, until: None });
        rec.pings = rtts.iter().enumerate().map(|(i, &rtt)| Ping { target: 1, t: i as f64 * 250.0, rtt }).collect();
        rec
    }

    #[test]
    fn ping_spikes_and_losses() {
        let mut rtts: Vec<Option<f32>> = (0..200).map(|i| Some(30.0 + (i % 4) as f32)).collect();
        rtts[50] = Some(48.0); // mild blip: not lag on its own
        rtts[100] = Some(260.0); // one huge spike: lag
        rtts[150] = None; // two losses close together: lag
        rtts[152] = None;
        rtts[190] = None; // single loss: noise
        let lag = find_lag(&pinged(&rtts));
        assert_eq!(lag.len(), 2, "{lag:?}");
        assert_eq!(lag[0].signs, vec![LagSign::Spike { rtt: 260.0, normal: 31.5 }]);
        assert_eq!(lag[1].signs, vec![LagSign::Loss { lost: 2 }]);
    }

    #[test]
    fn unpingable_server_is_not_lag() {
        let rec = pinged(&vec![None; 100]);
        assert!(path_targets(&rec).is_empty());
        assert!(find_lag(&rec).is_empty());
    }

    #[test]
    fn traffic_silence() {
        let mut rec = Recording::default();
        rec.game_traffic = (0..300)
            .map(|i| TrafficBin { packets_in: if (100..108).contains(&i) || (200..201).contains(&i) { 0 } else { 6 }, ..Default::default() })
            .collect();
        let lag = find_lag(&rec);
        assert_eq!(lag.len(), 1);
        assert_eq!(lag[0].signs, vec![LagSign::Silence { ms: 800.0 }]);
        assert_eq!(lag[0].t, 10_000.0);
    }
}
