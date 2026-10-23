//! A whole recording fed through the same steps the Windows recorder takes, from the raw
//! inputs it reads (PresentMon's CSV, network trace events, the kernel's process list) to
//! the finished report.

use culprit_capture::clock::Clock;
use culprit_capture::collector::{Collector, Event, Options, ProcReading, Tick};
use culprit_capture::frames::FrameFeed;
use culprit_capture::naming;
use culprit_capture::netevents;
use culprit_capture::procs::{ProcRates, RawProc};
use culprit_capture::targets::{Route, Targets};
use culpritcore::model::{Connection, LinkKind, Machine, SystemSample, TargetRole};
use std::net::IpAddr;

const HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,AllowsTearing,PresentMode,TimeInQPC,MsBetweenSimulationStart,MsBetweenPresents,MsBetweenDisplayChange,MsInPresentAPI,MsRenderPresentLatency,MsUntilDisplayed,CPUStartQPC,MsBetweenAppStart,MsCPUBusy,MsCPUWait,MsGPULatency,MsGPUTime,MsGPUBusy,MsGPUWait";
/// Counter ticks per millisecond on this made-up PC.
const TICKS_PER_MS: f64 = 10_000.0;
const GAME: u32 = 4120;
const DEFENDER: u32 = 3300;
const HITCH_SECONDS: [u64; 5] = [20, 35, 50, 65, 80];

fn row(app: &str, pid: u32, t_ms: f64, ms: f64) -> String {
    let qpc = (t_ms * TICKS_PER_MS) as u64;
    format!("{app},{pid},0x1,DXGI,0,0,1,Hardware: Independent Flip,{qpc},NA,{ms:.4},{ms:.4},0.1,4.0,5.0,{qpc},{ms:.4},3.5,1.0,0.5,4.0,3.9,0.1")
}

fn udp(pid: u32, remote: [u8; 4], port: u16, size: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&pid.to_le_bytes());
    v.extend_from_slice(&size.to_le_bytes());
    v.extend_from_slice(&remote);
    v.extend_from_slice(&[192, 168, 1, 20]);
    v.extend_from_slice(&port.to_be_bytes());
    v.extend_from_slice(&50_000u16.to_be_bytes());
    v.extend_from_slice(&[0; 8]);
    v
}

#[test]
fn raw_inputs_to_a_verdict() {
    let clock = Clock { origin: 0, freq: (TICKS_PER_MS * 1000.0) as u64 };
    let machine = Machine { logical_cpus: 8, mem_total_mb: Some(16_384.0), ..Default::default() };
    let mut c = Collector::new(
        Options::default(),
        1_790_000_000_000,
        machine,
        Connection { kind: LinkKind::Ethernet, ..Default::default() },
    );
    let mut targets = Targets::default();
    targets.set_router(&mut c, Some("192.168.1.1".parse().unwrap()), 0.0);
    let mut feed = FrameFeed::new(clock, 0.0);
    assert!(feed.line(HEADER).is_none());
    let mut rates = ProcRates::default();
    let (mut game_cpu, mut defender_cpu, mut discord_cpu) = (0u64, 0u64, 0u64);
    let server: IpAddr = "155.133.250.9".parse().unwrap();
    let mut route_pending = false;

    for s in 0..90u64 {
        let start = s as f64 * 1000.0;
        // The game at 144 fps, one long frame in each hitch second; Discord drawing too.
        let hitch = HITCH_SECONDS.contains(&s);
        for i in 0..144 {
            let ms = if hitch && i == 72 { 70.0 } else { 6.94 };
            let line = row("game.exe", GAME, start + i as f64 * 6.94, ms);
            if let Some((r, t)) = feed.line(&line) {
                c.on_frame(&r, t);
            }
        }
        for i in 0..60 {
            if let Some((r, t)) = feed.line(&row("Discord.exe", 77, start + i as f64 * 16.6, 16.6)) {
                c.on_frame(&r, t);
            }
        }
        // The game's traffic with its server, and a DNS lookup that isn't the server.
        for i in 0..64 {
            let t = start + i as f64 * 15.6;
            for (id, size) in [(42u16, 90u32), (43, 300)] {
                c.on_packet(netevents::parse(id, &udp(GAME, [155, 133, 250, 9], 27015, size), t).unwrap());
            }
        }
        c.on_packet(netevents::parse(42, &udp(GAME, [8, 8, 8, 8], 53, 40), start).unwrap());
        if route_pending {
            let route = Route {
                hops: vec![Some("192.168.1.1".parse().unwrap()), Some("84.116.0.1".parse().unwrap()), Some(server)],
                server_answers: true,
            };
            targets.set_route(&mut c, server, &route, start);
            route_pending = false;
        }
        for q in 0..4 {
            let t = start + q as f64 * 250.0;
            for (id, ip) in targets.active() {
                let rtt = if ip == server {
                    30.0
                } else if ip.to_string() == "84.116.0.1" {
                    9.0
                } else {
                    1.0
                };
                c.on_ping(id, t, Some(rtt));
            }
        }

        // The kernel's process list a second later: Defender bursts exactly when the game hitches.
        game_cpu += 24_000_000; // 30% of 8 threads for a second, in 100 ns units
        defender_cpu += if hitch { 32_000_000 } else { 200_000 };
        discord_cpu += 2_400_000;
        let list = [
            RawProc { pid: GAME, name: "game.exe".into(), created: 1, cpu_time: game_cpu, io_bytes: s * 300_000 },
            RawProc { pid: DEFENDER, name: "MsMpEng.exe".into(), created: 2, cpu_time: defender_cpu, io_bytes: 0 },
            RawProc { pid: 77, name: "Discord.exe".into(), created: 3, cpu_time: discord_cpu, io_bytes: 0 },
        ];
        let procs: Vec<ProcReading> = list
            .iter()
            .zip(rates.update(&list, 1.0, 8))
            .filter_map(|(p, rate)| {
                let rate = rate?;
                let (key, label, kind) = naming::identify(&p.name, &[], || None);
                Some(ProcReading { pid: p.pid, exe: p.name.clone(), key, label, kind, cpu: rate.cpu, io: rate.io })
            })
            .collect();
        let tick = Tick {
            t: start + 1000.0,
            dt: 1.0,
            foreground: vec![Some(GAME); 4],
            sample: SystemSample { cpu: Some(40.0), mem_free_mb: Some(8_000.0), ..Default::default() },
            procs,
        };
        for event in c.on_tick(tick) {
            match event {
                Event::GameLocked { exe, .. } => c.name_game(naming::game_name(&exe, Some("Test Game™"), None, None)),
                Event::ServerChanged { ip, port } => {
                    assert_eq!((ip, port), (server, 27015));
                    route_pending = true;
                }
            }
        }
    }

    let live = c.live(90_000.0);
    assert_eq!(live.stutters, 5);
    assert_eq!(live.server.as_deref(), Some("155.133.250.9"));
    let rec = c.finish(90_000.0);
    assert_eq!(rec.game.as_ref().unwrap().name, "Test Game");
    let roles: Vec<TargetRole> = rec.targets.iter().map(|t| t.role).collect();
    assert_eq!(roles, vec![TargetRole::Router, TargetRole::Isp, TargetRole::Server]);

    let session = culpritcore::summarize(&rec);
    let report = &session.report;
    assert_eq!(report.perf.as_ref().unwrap().stutters, 5);
    assert_eq!(report.headline, "Microsoft Defender caused all 5 of your stutters.");
    assert_eq!(report.net.as_ref().unwrap().server.as_deref(), Some("155.133.250.9"));
}
