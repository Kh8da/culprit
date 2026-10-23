//! Made-up sessions with a known cause, for the example report and for previewing the app.
//! Everything is deterministic, so the same example always reads the same way.

use crate::model::*;

struct Gen {
    rec: Recording,
    seed: u64,
    secs: usize,
}

/// A time window in seconds, `[from, to)`.
type Span = (f64, f64);

fn inside(t_ms: f64, spans: &[Span]) -> bool {
    let s = t_ms / 1000.0;
    spans.iter().any(|&(a, b)| s >= a && s < b)
}

impl Gen {
    fn new(started_at: u64, minutes: usize, game: &str, exe: &str, link: LinkKind) -> Self {
        let secs = minutes * 60;
        let rec = Recording {
            started_at,
            duration_ms: secs as f64 * 1000.0,
            machine: Machine {
                cpu: Some("AMD Ryzen 7 5800X3D".into()),
                logical_cpus: 16,
                mem_total_mb: Some(32_768.0),
                gpu: Some("NVIDIA GeForce RTX 3070".into()),
                vram_total_mb: Some(8_192.0),
                os: Some("Windows 11".into()),
            },
            game: Some(Game { name: game.into(), exe: exe.into(), pid: 4120, since: 0.0 }),
            connection: Connection {
                kind: link,
                adapter: Some(if link == LinkKind::Wifi { "Wi-Fi" } else { "Ethernet" }.into()),
                wifi_band_ghz: (link == LinkKind::Wifi).then_some(2.4),
                link_mbps: Some(if link == LinkKind::Wifi { 144.0 } else { 1000.0 }),
            },
            ..Default::default()
        };
        let mut g = Gen { rec, seed: started_at, secs };
        g.rec.system = (1..=secs)
            .map(|s| SystemSample {
                t: s as f64 * 1000.0,
                cpu: Some(38.0 + (s % 7) as f32),
                cpu_perf: Some(112.0),
                mem_free_mb: Some(17_000.0),
                hard_faults: Some(2.0),
                disk_busy: Some(3.0),
                gpu: Some(82.0),
                vram_used_mb: Some(5_600.0),
                gpu_temp: Some(68.0 + (s % 5) as f32),
                gpu_clock: Some(1_905.0),
                game_cpu: Some(30.0),
                game_io: Some(400_000.0),
                foreground: true,
                wifi_signal: (link == LinkKind::Wifi).then_some(58.0),
                net_in: 60_000.0,
                net_out: 25_000.0,
                ..Default::default()
            })
            .collect();
        g
    }

    fn rand(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.seed >> 40) as f32) / (1u64 << 24) as f32
    }

    /// Frames at `ms` each with a little jitter, plus hitches at given seconds with given
    /// lengths. `cpu_side` marks hitches where the game's own CPU work was the slow part.
    fn frames(&mut self, ms: f32, hitches: &[(f64, f32, bool)]) {
        let end = self.secs as f64 * 1000.0;
        let mut pending: Vec<(f64, f32, bool)> = hitches.iter().map(|&(s, m, c)| (s * 1000.0, m, c)).collect();
        pending.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut next = 0;
        let mut t = 0.0;
        self.rec.frames.reserve((end / ms as f64) as usize + 16);
        while t < end {
            let mut f = ms * (0.94 + self.rand() * 0.12);
            let mut cpu = ms * 0.55;
            let mut gpu = ms * 0.8;
            if next < pending.len() && t >= pending[next].0 {
                let (_, m, cpu_side) = pending[next];
                f = m;
                if cpu_side {
                    cpu = m * 0.88;
                } else {
                    cpu = ms * 0.6;
                    gpu = ms * 0.8;
                }
                next += 1;
            }
            t += f as f64;
            self.rec.frames.push(Frame { t, ms: f, cpu_busy: Some(cpu), gpu_busy: Some(gpu) });
        }
    }

    fn process(&mut self, key: &str, label: &str, kind: ProcessKind, f: impl Fn(&mut Self, usize) -> ProcSample) {
        let mut samples = Vec::new();
        for s in 1..=self.secs {
            let p = ProcSample { t: s as f64 * 1000.0, ..f(self, s) };
            if p.cpu >= 1.0 || p.io >= 1e6 || p.net_in >= 1e5 || p.net_out >= 1e5 {
                samples.push(p);
            }
        }
        self.rec.processes.push(ProcessTrack { key: key.into(), label: label.into(), kind, samples });
    }

    fn everyday_apps(&mut self) {
        self.process("discord.exe", "Discord", ProcessKind::App, |g, _| ProcSample {
            cpu: 1.5 + g.rand() * 2.0,
            net_in: 20_000.0,
            ..Default::default()
        });
        self.process("chrome.exe", "Google Chrome", ProcessKind::App, |g, s| ProcSample {
            cpu: if s % 41 == 0 { 9.0 } else { 1.0 + g.rand() * 2.5 },
            ..Default::default()
        });
        self.process("steamwebhelper.exe", "Steam (overlay and store)", ProcessKind::App, |g, _| ProcSample {
            cpu: 0.8 + g.rand() * 0.8,
            ..Default::default()
        });
    }

    fn target(&mut self, id: u32, role: TargetRole, ip: &str, rtt: impl Fn(&mut Self, f64) -> Option<f32>) {
        self.rec.targets.push(Target { id, role, ip: ip.into(), host: None, since: 0.0, until: None });
        for i in 0..self.secs * 4 {
            let t = i as f64 * 250.0;
            let r = rtt(self, t);
            self.rec.pings.push(Ping { target: id, t, rtt: r });
        }
    }

    fn server_traffic(&mut self, ip: &str, port: u16, quiet: &[Span]) {
        self.rec.servers.push(ServerSeen { t: 2_000.0, ip: ip.into(), port, protocol: "udp".into() });
        self.rec.game_traffic = (0..self.secs * 10)
            .map(|i| {
                let t = i as f64 * 100.0;
                let silent = inside(t, quiet);
                TrafficBin {
                    packets_in: if silent { 0 } else { 6 + (i % 2) as u32 },
                    packets_out: 6,
                    bytes_in: if silent { 0 } else { 1_800 },
                    bytes_out: 600,
                }
            })
            .collect();
    }

    fn finish(mut self) -> Recording {
        self.rec.pings.sort_by(|a, b| a.t.total_cmp(&b.t));
        self.rec
    }
}

/// Oct 22 2026, 21:14 UTC, and earlier sessions counting back from it.
const BASE: u64 = 1_792_703_640_000;
const HOUR: u64 = 3_600_000;

/// Microsoft Defender scanning in the middle of a match, plus a few shader hitches early on.
pub fn defender() -> Recording {
    let mut g = Gen::new(BASE, 32, "Counter-Strike 2", "cs2.exe", LinkKind::Ethernet);
    let scan: Vec<Span> = vec![
        (731.0, 733.0),
        (746.0, 748.0),
        (759.0, 761.0),
        (777.0, 779.0),
        (1490.0, 1492.0),
        (1508.0, 1510.0),
        (1527.0, 1529.0),
        (1541.0, 1543.0),
        (1560.0, 1562.0),
    ];
    let mut hitches: Vec<(f64, f32, bool)> =
        scan.iter().map(|&(a, _)| (a + 0.4, 70.0 + (a % 9.0) as f32 * 6.0, false)).collect();
    hitches.extend([(41.2, 48.0, true), (95.7, 55.0, true)]);
    g.frames(6.06, &hitches);
    let busy = scan.clone();
    g.process("msmpeng.exe", "Microsoft Defender", ProcessKind::App, move |g, s| ProcSample {
        cpu: if inside(s as f64 * 1000.0 - 1.0, &busy) { 34.0 + g.rand() * 10.0 } else { 0.4 },
        io: if inside(s as f64 * 1000.0 - 1.0, &busy) { 45_000_000.0 } else { 0.0 },
        ..Default::default()
    });
    g.everyday_apps();
    g.target(1, TargetRole::Router, "192.168.1.1", |g, _| Some(0.8 + g.rand() * 0.6));
    g.target(2, TargetRole::Isp, "84.116.0.1", |g, _| Some(7.5 + g.rand() * 2.0));
    g.target(3, TargetRole::Server, "155.133.252.38", |g, _| Some(27.0 + g.rand() * 3.0));
    g.server_traffic("155.133.252.38", 27026, &[]);
    g.rec.marks = vec![1_545_200.0];
    g.finish()
}

/// Lag on 2.4 GHz Wi-Fi with a weak signal, spiking about once a minute.
pub fn wifi() -> Recording {
    let mut g = Gen::new(BASE - 26 * HOUR, 24, "Apex Legends", "r5apex.exe", LinkKind::Wifi);
    g.frames(7.4, &[(300.5, 52.0, true)]);
    let spikes: Vec<Span> =
        (0..11).map(|i| 95.0 + i as f64 * 61.0 + (i % 3) as f64 * 1.5).map(|s| (s, s + 1.2)).collect();
    for s in g.rec.system.iter_mut() {
        let near = inside(s.t - 500.0, &spikes);
        s.wifi_signal = Some(if near { 34.0 } else { 52.0 + ((s.t / 1000.0) as u64 % 9) as f32 });
    }
    let (a, b, c) = (spikes.clone(), spikes.clone(), spikes.clone());
    g.target(1, TargetRole::Router, "192.168.0.1", move |g, t| {
        if inside(t, &a) {
            (g.rand() > 0.3).then_some(140.0 + g.rand() * 120.0)
        } else {
            Some(3.0 + g.rand() * 3.0)
        }
    });
    g.target(2, TargetRole::Isp, "62.252.170.1", move |g, t| {
        Some(if inside(t, &b) { 180.0 + g.rand() * 100.0 } else { 11.0 + g.rand() * 3.0 })
    });
    g.target(3, TargetRole::Server, "104.198.81.13", move |g, t| {
        Some(if inside(t, &c) { 210.0 + g.rand() * 110.0 } else { 34.0 + g.rand() * 4.0 })
    });
    g.server_traffic("104.198.81.13", 37005, &[]);
    g.everyday_apps();
    g.finish()
}

/// A bad night on the game server: slow answers and the server going quiet, nothing wrong at home.
pub fn server() -> Recording {
    let mut g = Gen::new(BASE - 50 * HOUR, 21, "Valorant", "VALORANT-Win64-Shipping.exe", LinkKind::Ethernet);
    g.frames(4.2, &[]);
    let slow: Vec<Span> = vec![(212.0, 213.0), (488.0, 489.5), (803.0, 804.0), (1004.0, 1005.0)];
    let quiet: Vec<Span> = vec![(655.0, 655.9), (1121.0, 1122.2)];
    g.target(1, TargetRole::Router, "192.168.1.1", |g, _| Some(0.7 + g.rand() * 0.5));
    g.target(2, TargetRole::Isp, "84.116.0.1", |g, _| Some(8.0 + g.rand() * 2.0));
    let s2 = slow.clone();
    g.target(3, TargetRole::Server, "162.249.72.1", move |g, t| {
        Some(if inside(t, &s2) { 240.0 + g.rand() * 160.0 } else { 19.0 + g.rand() * 2.5 })
    });
    g.server_traffic("162.249.72.1", 7104, &quiet);
    g.everyday_apps();
    g.finish()
}

/// A single-player evening with nothing wrong.
pub fn clean() -> Recording {
    let mut g = Gen::new(BASE - 74 * HOUR, 47, "Elden Ring", "eldenring.exe", LinkKind::Ethernet);
    g.frames(16.67, &[]);
    g.target(1, TargetRole::Router, "192.168.1.1", |g, _| Some(0.7 + g.rand() * 0.5));
    g.everyday_apps();
    g.finish()
}

/// A hot GPU throttling itself once the room warms up, plus Windows Update.
pub fn hot_gpu() -> Recording {
    let mut g = Gen::new(BASE - 98 * HOUR, 41, "Cyberpunk 2077", "Cyberpunk2077.exe", LinkKind::Ethernet);
    let hot: Vec<Span> = (0..9).map(|i| 1_020.0 + i as f64 * 140.0).map(|s| (s, s + 3.0)).collect();
    let update: Vec<Span> = vec![(610.0, 612.0), (655.0, 657.0), (702.0, 704.0)];
    let mut hitches: Vec<(f64, f32, bool)> =
        hot.iter().map(|&(a, _)| (a + 1.1, 64.0 + (a % 7.0) as f32 * 5.0, false)).collect();
    hitches.extend(update.iter().map(|&(a, _)| (a + 0.5, 58.0, false)));
    g.frames(14.3, &hitches);
    for s in g.rec.system.iter_mut() {
        let minute = s.t / 60_000.0;
        s.gpu_temp = Some((66.0 + minute as f32 * 0.7).min(87.0));
        if inside(s.t - 1.0, &hot) {
            s.gpu_throttle = Some(GpuThrottle { thermal: true, ..Default::default() });
            s.gpu_temp = Some(91.0);
            s.gpu_clock = Some(1_410.0);
        }
    }
    let up = update.clone();
    g.process("svc:wuauserv", "Windows Update", ProcessKind::Service, move |g, s| ProcSample {
        cpu: if inside(s as f64 * 1000.0 - 1.0, &up) { 22.0 + g.rand() * 8.0 } else { 0.0 },
        io: if inside(s as f64 * 1000.0 - 1.0, &up) { 30_000_000.0 } else { 0.0 },
        ..Default::default()
    });
    g.everyday_apps();
    g.target(1, TargetRole::Router, "192.168.1.1", |g, _| Some(0.7 + g.rand() * 0.5));
    g.finish()
}

/// Every example, newest first.
pub fn all() -> Vec<Recording> {
    vec![defender(), wifi(), server(), clean(), hot_gpu()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;

    #[test]
    fn examples_read_as_intended() {
        let headlines: Vec<String> = all().iter().map(|r| analyze(r).headline).collect();
        assert_eq!(
            headlines,
            vec![
                "Microsoft Defender caused 9 of your 11 stutters.",
                "Your Wi-Fi caused all 11 of your lag spikes.",
                "The game server caused all 6 of your lag spikes.",
                "No stutters or lag spikes.",
                "Your GPU throttled itself: 9 of your 12 stutters.",
            ]
        );
    }
}
