//! Whole sessions built from scratch, each with a known culprit, run through the analyzer.

use culpritcore::model::*;
use culpritcore::report::{Cause, Confidence, IncidentKind, Mood};
use culpritcore::{analyze, summarize};

struct Sim {
    rec: Recording,
    seed: u64,
    secs: usize,
}

impl Sim {
    fn new(secs: usize) -> Self {
        let mut rec = Recording {
            started_at: 1_790_000_000_000,
            duration_ms: secs as f64 * 1000.0,
            machine: Machine {
                logical_cpus: 8,
                mem_total_mb: Some(16_000.0),
                vram_total_mb: Some(8_192.0),
                ..Default::default()
            },
            game: Some(Game { name: "Counter-Strike 2".into(), exe: "cs2.exe".into(), pid: 4120, since: 0.0 }),
            connection: Connection { kind: LinkKind::Ethernet, ..Default::default() },
            ..Default::default()
        };
        rec.system = (1..=secs)
            .map(|s| SystemSample {
                t: s as f64 * 1000.0,
                cpu: Some(45.0),
                cpu_perf: Some(110.0),
                mem_free_mb: Some(6_000.0),
                hard_faults: Some(3.0),
                gpu: Some(80.0),
                vram_used_mb: Some(5_000.0),
                gpu_temp: Some(70.0),
                game_cpu: Some(35.0),
                game_io: Some(200_000.0),
                foreground: true,
                ..Default::default()
            })
            .collect();
        Sim { rec, seed: 7, secs }
    }

    fn rand(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.seed >> 33) as f32) / (u32::MAX >> 1) as f32
    }

    /// Frames at `ms` each, with small jitter, plus a hitch of `hitch_ms` at every time in `hitches`.
    fn frames(&mut self, ms: f32, hitches: &[(f64, f32)], cpu_heavy: bool) {
        let end = self.secs as f64 * 1000.0;
        let mut t = 0.0;
        let mut pending: Vec<(f64, f32)> = hitches.to_vec();
        while t < end {
            let mut f = ms * (0.95 + self.rand() * 0.1);
            if let Some(pos) = pending.iter().position(|(h, _)| t >= *h) {
                f = pending.remove(pos).1;
            }
            t += f as f64;
            let cpu_busy = if cpu_heavy && f > ms * 2.0 { Some(f * 0.85) } else { Some(ms * 0.6) };
            self.rec.frames.push(Frame { t, ms: f, cpu_busy, gpu_busy: Some(ms * 0.5) });
        }
    }

    fn process(&mut self, key: &str, label: &str, f: impl Fn(usize) -> ProcSample) {
        let samples = (1..=self.secs)
            .map(|s| ProcSample { t: s as f64 * 1000.0, ..f(s) })
            .filter(|p| p.cpu >= 1.0 || p.io >= 1e6 || p.net_in >= 1e5 || p.net_out >= 1e5)
            .collect();
        self.rec.processes.push(ProcessTrack { key: key.into(), label: label.into(), kind: ProcessKind::App, samples });
    }

    fn target(&mut self, id: u32, role: TargetRole, ip: &str, rtt: impl Fn(f64) -> Option<f32>) {
        self.rec.targets.push(Target { id, role, ip: ip.into(), host: None, since: 0.0, until: None });
        let n = self.secs * 4;
        for i in 0..n {
            let t = i as f64 * 250.0;
            let jitter = self.rand() * 2.0;
            self.rec.pings.push(Ping { target: id, t, rtt: rtt(t).map(|r| r + jitter) });
        }
    }

    fn traffic(&mut self, silent: impl Fn(f64) -> bool) {
        self.rec.servers.push(ServerSeen { t: 0.0, ip: "155.133.250.9".into(), port: 27015, protocol: "udp".into() });
        self.rec.game_traffic = (0..self.secs * 10)
            .map(|i| {
                let quiet = silent(i as f64 * 100.0);
                TrafficBin { packets_in: if quiet { 0 } else { 6 }, packets_out: 6, bytes_in: 600, bytes_out: 400 }
            })
            .collect();
    }
}

fn near(t: f64, spots: &[f64], width: f64) -> bool {
    spots.iter().any(|s| t >= *s && t < s + width)
}

#[test]
fn defender_scans_cause_the_stutters() {
    let mut sim = Sim::new(600);
    let scans: Vec<f64> = (0..9).map(|i| 30_000.0 + i as f64 * 61_000.0 + (i % 3) as f64 * 7_000.0).collect();
    let hitches: Vec<(f64, f32)> = scans.iter().map(|s| (s + 400.0, 85.0)).collect();
    sim.frames(6.9, &hitches, false);
    let scan_secs = scans.clone();
    sim.process("msmpeng.exe", "Microsoft Defender", move |s| ProcSample {
        cpu: if near(s as f64 * 1000.0 - 1.0, &scan_secs, 2_000.0) { 38.0 } else { 0.5 },
        ..Default::default()
    });
    sim.process("discord.exe", "Discord", |_| ProcSample { cpu: 3.0, ..Default::default() });
    // Chrome bursts at random times that don't line up with the hitches.
    sim.process("chrome.exe", "Google Chrome", |s| ProcSample {
        cpu: if s % 37 == 0 { 25.0 } else { 2.0 },
        ..Default::default()
    });
    let report = analyze(&sim.rec);
    if std::env::var_os("CULPRIT_PRINT").is_some() {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }

    assert_eq!(report.perf.as_ref().unwrap().stutters, 9, "{:#?}", report.incidents);
    assert_eq!(report.headline, "Microsoft Defender caused all 9 of your stutters.");
    let top = &report.culprits[0];
    assert_eq!(top.cause, Cause::App { key: "msmpeng.exe".into(), label: "Microsoft Defender".into() });
    assert_eq!(top.confidence, Confidence::High);
    assert_eq!(
        top.why,
        "Microsoft Defender was in a CPU burst during 9 of 9 stutters, and only 2% of the rest of the time."
    );
    assert!(top.fix.contains("Task Scheduler"));
    assert!(
        report.incidents[0].evidence[0].contains("jumped to 38% of your CPU"),
        "{:?}",
        report.incidents[0].evidence
    );
    assert_eq!(report.mood, Mood::Fair);
}

#[test]
fn the_game_itself_when_nothing_else_is_busy() {
    let mut sim = Sim::new(300);
    let hitches: Vec<(f64, f32)> = (0..12).map(|i| (10_000.0 + i as f64 * 23_000.0, 120.0)).collect();
    sim.frames(8.0, &hitches, true);
    sim.process("discord.exe", "Discord", |_| ProcSample { cpu: 3.0, ..Default::default() });
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "Most stutters came from the game itself, not your PC.");
    assert_eq!(report.culprits[0].cause, Cause::GameCpu);
    assert!(report.incidents[0].evidence[0].contains("The game's own CPU work took 102 ms of that 120 ms frame"));
}

#[test]
fn wifi_router_lag() {
    let mut sim = Sim::new(300);
    sim.rec.connection = Connection { kind: LinkKind::Wifi, wifi_band_ghz: Some(2.4), ..Default::default() };
    sim.frames(6.9, &[], false);
    let bad: Vec<f64> = (0..6).map(|i| 20_000.0 + i as f64 * 45_000.0).collect();
    let (b1, b2) = (bad.clone(), bad.clone());
    sim.target(1, TargetRole::Router, "192.168.1.1", move |t| Some(if near(t, &b1, 1_000.0) { 220.0 } else { 3.0 }));
    sim.target(2, TargetRole::Isp, "80.10.0.1", move |t| Some(if near(t, &b2, 1_000.0) { 240.0 } else { 11.0 }));
    let b3 = bad.clone();
    sim.target(3, TargetRole::Server, "155.133.250.9", move |t| Some(if near(t, &b3, 1_000.0) { 260.0 } else { 32.0 }));
    sim.traffic(|_| false);
    for s in sim.rec.system.iter_mut() {
        s.wifi_signal = Some(if near(s.t - 1000.0, &bad, 1_500.0) { 35.0 } else { 62.0 });
    }
    let report = analyze(&sim.rec);
    assert_eq!(report.net.as_ref().unwrap().lag_spikes, 6, "{:#?}", report.incidents);
    assert_eq!(report.headline, "Your Wi-Fi caused all 6 of your lag spikes.");
    let inc = &report.incidents[0];
    assert_eq!(inc.kind, IncidentKind::Lag);
    assert!(inc.what.starts_with("Ping to the server jumped to"), "{}", inc.what);
    assert!(inc.evidence.iter().any(|e| e.starts_with("Your router (over Wi-Fi) took")), "{:?}", inc.evidence);
    assert!(inc.evidence.iter().any(|e| e.contains("signal was weak (35%)")), "{:?}", inc.evidence);
    let net = report.net.unwrap();
    assert_eq!(net.router_ms.map(|v| v.round()), Some(4.0));
    assert_eq!(net.server.as_deref(), Some("155.133.250.9"));
}

#[test]
fn provider_lag_when_router_is_fine() {
    let mut sim = Sim::new(300);
    sim.frames(6.9, &[], false);
    let bad: Vec<f64> = (0..5).map(|i| 40_000.0 + i as f64 * 50_000.0).collect();
    let (b2, b3) = (bad.clone(), bad.clone());
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(2, TargetRole::Isp, "80.10.0.1", move |t| if near(t, &b2, 1_000.0) { None } else { Some(9.0) });
    sim.target(3, TargetRole::Server, "155.133.250.9", move |t| if near(t, &b3, 1_000.0) { None } else { Some(30.0) });
    sim.traffic(|_| false);
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "Your internet provider caused all 5 of your lag spikes.");
    let inc = &report.incidents[0];
    assert!(inc.what.contains("pings to the server got no answer"), "{}", inc.what);
    assert!(
        inc.evidence.iter().any(|e| e.starts_with("Your provider's first hop didn't answer")),
        "{:?}",
        inc.evidence
    );
    assert!(inc.evidence.iter().any(|e| e.starts_with("Your router answered normally")), "{:?}", inc.evidence);
}

#[test]
fn server_lag_and_silence() {
    let mut sim = Sim::new(300);
    sim.frames(6.9, &[], false);
    let bad: Vec<f64> = (0..4).map(|i| 30_000.0 + i as f64 * 60_000.0).collect();
    let quiet: Vec<f64> = vec![100_000.0, 200_000.0];
    let b3 = bad.clone();
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(2, TargetRole::Isp, "80.10.0.1", |_| Some(9.0));
    sim.target(3, TargetRole::Server, "155.133.250.9", move |t| Some(if near(t, &b3, 750.0) { 300.0 } else { 30.0 }));
    sim.traffic(move |t| near(t, &quiet, 800.0));
    let report = analyze(&sim.rec);
    assert_eq!(report.net.as_ref().unwrap().lag_spikes, 6, "{:#?}", report.incidents);
    assert_eq!(report.headline, "The game server caused all 6 of your lag spikes.");
    assert!(report.incidents.iter().any(|i| i.what == "No data from the server for 800 ms"));
    assert!(report.incidents.iter().all(|i| i.confidence == Confidence::High));
}

#[test]
fn unpingable_server_falls_back_to_last_hop() {
    let mut sim = Sim::new(200);
    sim.frames(6.9, &[], false);
    let bad = vec![50_000.0, 120_000.0];
    let b = bad.clone();
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(3, TargetRole::Server, "155.133.250.9", |_| None);
    sim.target(4, TargetRole::ServerPath, "62.115.1.1", move |t| Some(if near(t, &b, 750.0) { 250.0 } else { 28.0 }));
    sim.traffic(|_| false);
    let report = analyze(&sim.rec);
    assert_eq!(report.net.as_ref().unwrap().lag_spikes, 2);
    assert!(report.incidents[0].what.starts_with("Ping to the server's network jumped to"));
    assert!(!report.net.as_ref().unwrap().server_answers_ping);
    assert!(report.notes.iter().any(|n| n.contains("doesn't answer pings")));
}

#[test]
fn steam_download_causes_lag() {
    let mut sim = Sim::new(400);
    sim.frames(6.9, &[], false);
    // Steam downloads in three windows; lag spikes land inside them.
    let downloads = vec![50_000.0, 180_000.0, 300_000.0];
    let bad: Vec<f64> = downloads.iter().flat_map(|d| [d + 5_000.0, d + 20_000.0]).collect();
    let dl = downloads.clone();
    sim.process("steam.exe", "Steam", move |s| ProcSample {
        net_in: if near(s as f64 * 1000.0, &dl, 30_000.0) { 6_000_000.0 } else { 0.0 },
        cpu: 2.0,
        ..Default::default()
    });
    let (b2, b3) = (bad.clone(), bad.clone());
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(2, TargetRole::Isp, "80.10.0.1", move |t| Some(if near(t, &b2, 750.0) { 180.0 } else { 9.0 }));
    sim.target(3, TargetRole::Server, "155.133.250.9", move |t| Some(if near(t, &b3, 750.0) { 210.0 } else { 30.0 }));
    sim.traffic(|_| false);
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "Steam caused all 6 of your lag spikes.", "{:#?}", report.culprits);
    assert!(
        report.incidents[0].evidence[0].contains("Steam was downloading at 48 Mbit/s"),
        "{:?}",
        report.incidents[0].evidence
    );
    assert!(report.culprits[0].fix.contains("Allow downloads during gameplay"));
}

#[test]
fn hot_gpu_and_a_second_actionable_cause() {
    let mut sim = Sim::new(600);
    let hot: Vec<f64> = (0..8).map(|i| 60_000.0 + i as f64 * 60_000.0).collect();
    let mut hitches: Vec<(f64, f32)> = hot.iter().map(|h| (h + 500.0, 70.0)).collect();
    let updates = vec![95_000.0, 215_000.0, 335_000.0];
    hitches.extend(updates.iter().map(|u| (u + 300.0, 60.0)));
    hitches.sort_by(|a, b| a.0.total_cmp(&b.0));
    sim.frames(12.0, &hitches, false);
    for s in sim.rec.system.iter_mut() {
        if near(s.t - 1000.0, &hot, 1_500.0) {
            s.gpu_throttle = Some(GpuThrottle { thermal: true, ..Default::default() });
            s.gpu_temp = Some(91.0);
        }
    }
    let up = updates.clone();
    sim.process("svc:wuauserv", "Windows Update", move |s| ProcSample {
        cpu: if near(s as f64 * 1000.0 - 1.0, &up, 2_000.0) { 30.0 } else { 0.0 },
        ..Default::default()
    });
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "Your GPU throttled itself: 8 of your 11 stutters.", "{:#?}", report.culprits);
    assert!(report.detail.contains("Windows Update caused 3 more."), "{}", report.detail);
    assert!(report.incidents.iter().any(|i| i.evidence[0] == "The GPU was slowing itself down to cool off at 91°C."));
}

#[test]
fn clean_session() {
    let mut sim = Sim::new(120);
    sim.frames(6.9, &[], false);
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(3, TargetRole::Server, "155.133.250.9", |_| Some(30.0));
    sim.traffic(|_| false);
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "No stutters or lag spikes.");
    assert_eq!(report.mood, Mood::Good);
    assert!(report.detail.starts_with("Counter-Strike 2 averaged 145 fps"), "{}", report.detail);
    assert!(report.detail.ends_with("Ping to the server was 31 ms."), "{}", report.detail);
    assert!(report.culprits.is_empty());
}

#[test]
fn no_game_seen() {
    let mut sim = Sim::new(30);
    sim.rec.game = None;
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "Culprit didn't catch a game.");
}

#[test]
fn hotkey_marks() {
    let mut sim = Sim::new(120);
    sim.frames(16.7, &[(40_000.0, 110.0)], false);
    sim.rec.marks = vec![41_500.0, 90_000.0];
    let report = analyze(&sim.rec);
    let stutter = report.incidents.iter().find(|i| i.kind == IncidentKind::Stutter).unwrap();
    assert!(stutter.marked);
    let lone = report.incidents.iter().find(|i| i.kind == IncidentKind::Marked).unwrap();
    assert_eq!(lone.t, 90_000.0);
    assert_eq!(lone.cause, Cause::Unknown);
    assert!(lone.evidence[0].starts_with("The slowest frame in the 5 seconds before took"));
    // Marks don't count as stutters. The hitch had idle CPU and GPU, so it stays unexplained.
    assert_eq!(report.headline, "No clear cause on your PC for your only stutter.");
    assert!(report.culprits.iter().all(|c| c.kind != IncidentKind::Marked));
}

#[test]
fn sessions_round_trip_through_json() {
    let mut sim = Sim::new(60);
    sim.frames(6.9, &[(20_000.0, 80.0)], false);
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    let session = summarize(&sim.rec);
    let json = serde_json::to_string(&session).unwrap();
    let back: culpritcore::Session = serde_json::from_str(&json).unwrap();
    assert_eq!(back, session);
    assert!(json.len() < 200_000, "{}", json.len());
    let rec_json = serde_json::to_string(&sim.rec).unwrap();
    let rec_back: Recording = serde_json::from_str(&rec_json).unwrap();
    assert_eq!(rec_back.frames.len(), sim.rec.frames.len());
}

#[test]
fn alt_tab_back_in_is_not_a_stutter() {
    // The game runs at 144 fps, drops to 10 fps while in the background for 5 seconds,
    // then comes back. The slow frames right after returning aren't stutters.
    let mut sim = Sim::new(60);
    let mut t = 0.0;
    while t < 60_000.0 {
        let background = (30_000.0..35_000.0).contains(&t);
        let ms = if background || (35_000.0..35_300.0).contains(&t) { 100.0 } else { 6.9 };
        t += ms as f64;
        sim.rec.frames.push(Frame { t, ms, cpu_busy: None, gpu_busy: None });
    }
    for s in sim.rec.system.iter_mut() {
        s.foreground = !(31_000.0..=35_000.0).contains(&s.t);
    }
    let report = analyze(&sim.rec);
    assert_eq!(report.perf.as_ref().unwrap().stutters, 0, "{:#?}", report.incidents);
    assert_eq!(report.headline, "No stutters or lag spikes.");
}

#[test]
fn match_ending_is_not_lag() {
    // In a match the server sends 60 packets a second. When the match ends it goes quiet
    // for two seconds, then the lobby trickles in at a few packets a second.
    let mut sim = Sim::new(120);
    sim.frames(6.9, &[], false);
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(3, TargetRole::Server, "155.133.250.9", |_| Some(30.0));
    sim.traffic(|_| false);
    for (i, b) in sim.rec.game_traffic.iter_mut().enumerate() {
        let t = i as f64 * 100.0;
        b.packets_in = if t < 60_000.0 {
            6
        } else if t < 62_000.0 {
            0
        } else if i % 5 == 0 {
            1
        } else {
            0
        };
    }
    let report = analyze(&sim.rec);
    assert_eq!(report.net.as_ref().unwrap().lag_spikes, 0, "{:#?}", report.incidents);
}

#[test]
fn mixed_session_mentions_the_other_kind() {
    let mut sim = Sim::new(300);
    let hitches: Vec<(f64, f32)> = (0..6).map(|i| (20_000.0 + i as f64 * 40_000.0, 90.0)).collect();
    sim.frames(6.9, &hitches, true);
    let bad = vec![50_000.0, 150_000.0];
    let b = bad.clone();
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(2, TargetRole::Isp, "80.10.0.1", |_| Some(9.0));
    sim.target(3, TargetRole::Server, "155.133.250.9", move |t| Some(if near(t, &b, 750.0) { 300.0 } else { 30.0 }));
    sim.traffic(|_| false);
    let report = analyze(&sim.rec);
    assert_eq!(report.headline, "Most stutters came from the game itself, not your PC.");
    assert!(report.detail.contains("2 of the 2 lag spikes came from the game server."), "{}", report.detail);
}

#[test]
fn long_sessions_analyze_fast() {
    // Three hours at 240 fps with background apps and pings: the analysis has to stay quick.
    let secs = 3 * 3600;
    let mut sim = Sim::new(secs);
    let hitches: Vec<(f64, f32)> = (0..400).map(|i| (5_000.0 + i as f64 * 26_000.0, 60.0)).collect();
    sim.frames(4.2, &hitches, false);
    for k in 0..120 {
        let key = format!("app{k}.exe");
        sim.process(&key, &key, move |s| ProcSample {
            cpu: if (s + k) % 97 == 0 { 20.0 } else { 1.5 },
            ..Default::default()
        });
    }
    sim.target(1, TargetRole::Router, "192.168.1.1", |_| Some(1.0));
    sim.target(
        2,
        TargetRole::Isp,
        "80.10.0.1",
        |t| if (t as u64 / 250).is_multiple_of(300) { None } else { Some(9.0) },
    );
    sim.target(3, TargetRole::Server, "155.133.250.9", |t| {
        Some(if (t as u64 / 250).is_multiple_of(500) { 400.0 } else { 30.0 })
    });
    sim.traffic(|_| false);
    assert!(sim.rec.frames.len() > 2_500_000);
    let started = std::time::Instant::now();
    let session = summarize(&sim.rec);
    let took = started.elapsed();
    // Debug builds are ~10x slower than release; this bound holds for both.
    assert!(took.as_secs_f64() < 20.0, "took {took:?}");
    assert!(session.report.perf.as_ref().unwrap().stutters >= 400);
    eprintln!("3-hour session analyzed in {took:?}");
}
