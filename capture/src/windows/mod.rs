//! The Windows recorder. Everything here reads what Windows already reports to any
//! monitoring tool: nothing is injected into the game, no process is opened, and the only
//! packets sent are pings.
//!
//! - Frames come from PresentMon, Intel's frame tracer, run as a separate program.
//! - Packets come from Windows' own network trace, which says which process sent or
//!   received how many bytes to which address.
//! - Once a second the PC is sampled: CPU, memory, disk, GPU, Wi-Fi and every process.
//! - Pings go to the router, the provider's first hop and the game server.
//! - Ctrl+Alt+L marks a moment, through a registered hotkey (no keyboard hook).

mod elevate;
mod etw;
mod gpu;
mod hotkey;
mod net;
mod presentmon;
mod system;
mod util;

pub use elevate::{elevated, relaunch_elevated, wait_for_exit};

use crate::clock::Clock;
use crate::collector::{Collector, Event, Live, Options, Tick};
use crate::naming;
use crate::targets::Targets;
use culpritcore::Recording;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What every part of the recorder writes into.
pub(crate) struct State {
    pub collector: Collector,
    pub targets: Targets,
}

pub(crate) struct Shared {
    state: Mutex<Option<State>>,
    pub clock: Clock,
    pub stop: AtomicBool,
    /// Bumped on each server change, so a slow traceroute for an old server is ignored.
    route_generation: AtomicU64,
}

impl Shared {
    /// Milliseconds since the recording started.
    pub fn now(&self) -> f64 {
        self.clock.ms(util::qpc())
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Runs `f` on the recording's state; `None` once the recording has been handed over.
    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_mut().map(f)
    }

    pub fn note(&self, text: &str) {
        self.with(|s| s.collector.note(text));
    }
}

pub struct Recorder {
    shared: Arc<Shared>,
    frames: presentmon::FrameCapture,
    net: Option<etw::NetTrace>,
    hotkey: Option<hotkey::Hotkey>,
    threads: Vec<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(opts: Options) -> Result<Self, String> {
        if !elevated() {
            return Err("Recording needs administrator rights.".into());
        }
        let clock = Clock { origin: util::qpc(), freq: util::qpc_freq() };
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let gpu = gpu::primary_adapter();
        let machine = system::machine(gpu.as_ref());
        let link = net::link();
        let collector = Collector::new(opts, started_at, machine.clone(), link.connection.clone());
        let shared = Arc::new(Shared {
            state: Mutex::new(Some(State { collector, targets: Targets::default() })),
            clock,
            stop: AtomicBool::new(false),
            route_generation: AtomicU64::new(0),
        });

        // Without frames there is no game to lock on to, so this one has to work.
        let frames = presentmon::FrameCapture::start(shared.clone())?;

        let net = match etw::NetTrace::start(shared.clone()) {
            Ok(n) => Some(n),
            Err(e) => {
                eprintln!("network trace: {e}");
                shared.note("Culprit couldn't watch network traffic on this PC, so it couldn't tell which server the game used or which apps were using the connection.");
                None
            }
        };
        match link.gateway {
            Some(ip) => {
                shared.with(|s| s.targets.set_router(&mut s.collector, Some(ip), 0.0));
            }
            None => shared.note("Culprit couldn't find your router, so it couldn't check the link to it."),
        }
        let hotkey = match hotkey::Hotkey::start(shared.clone()) {
            Ok(h) => Some(h),
            Err(_) => {
                shared.note("Another app already uses Ctrl+Alt+L, so the hotkey didn't work this session.");
                None
            }
        };

        let sampler = system::Sampler::new(machine.logical_cpus, gpu, link.wifi);
        let threads = vec![
            spawn("culprit pings", {
                let shared = shared.clone();
                move || net::run_pinger(&shared)
            }),
            spawn("culprit sampler", {
                let shared = shared.clone();
                move || run_ticks(&shared, sampler, link)
            }),
        ];
        Ok(Recorder { shared, frames, net, hotkey, threads })
    }

    pub fn live(&self) -> Live {
        let now = self.shared.now();
        self.shared.with(|s| s.collector.live(now)).expect("the recording is only handed over by stop")
    }

    pub fn mark(&self) {
        let now = self.shared.now();
        self.shared.with(|s| s.collector.mark(now));
    }

    pub fn stop(self) -> Recording {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.frames.stop();
        if let Some(n) = self.net {
            n.stop();
        }
        if let Some(h) = self.hotkey {
            h.stop();
        }
        for t in self.threads {
            util::join_within(t, Duration::from_secs(3));
        }
        let now = self.shared.now();
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).take();
        state.expect("stop runs once").collector.finish(now)
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new().name(name.into()).spawn(f).expect("couldn't start a recorder thread")
}

/// Samples the foreground window four times a second and the whole PC once a second.
fn run_ticks(shared: &Arc<Shared>, mut sampler: system::Sampler, mut link: net::Link) {
    const STEP_MS: f64 = 250.0;
    let mut foreground = Vec::with_capacity(4);
    sampler.prime();
    let mut last_tick = shared.now();
    let mut next = last_tick + STEP_MS;
    let mut ticks = 0u32;
    while !shared.stopping() {
        let now = shared.now();
        if now < next {
            std::thread::sleep(Duration::from_secs_f64(((next - now) / 1000.0).min(0.25)));
            continue;
        }
        next = if now - next > 1_000.0 { now + STEP_MS } else { next + STEP_MS };
        foreground.push(sampler.foreground());
        if now - last_tick < 1_000.0 - STEP_MS / 2.0 {
            continue;
        }
        let dt = (now - last_tick) / 1000.0;
        let (sample, procs) = sampler.sample(dt);
        for note in sampler.take_notes() {
            shared.note(&note);
        }
        let tick = Tick { t: now, dt, foreground: std::mem::take(&mut foreground), sample, procs };
        last_tick = now;
        let events = shared.with(|s| s.collector.on_tick(tick)).unwrap_or_default();
        for event in events {
            match event {
                Event::GameLocked { pid, exe } => {
                    let (description, product) = sampler.version_strings(pid);
                    let title = system::window_title(pid);
                    let name = naming::game_name(&exe, description.as_deref(), product.as_deref(), title.as_deref());
                    shared.with(|s| s.collector.name_game(name));
                }
                Event::ServerChanged { ip, .. } => {
                    let generation = shared.route_generation.fetch_add(1, Ordering::Relaxed) + 1;
                    let shared = shared.clone();
                    spawn("culprit traceroute", move || {
                        let route = net::route_to(ip, &shared);
                        if shared.route_generation.load(Ordering::Relaxed) == generation && !shared.stopping() {
                            let t = shared.now();
                            shared.with(|s| s.targets.set_route(&mut s.collector, ip, &route, t));
                        }
                    });
                }
            }
        }
        // Notice a switch between Wi-Fi and cable, or a new router.
        ticks += 1;
        if ticks.is_multiple_of(10) {
            let fresh = net::link();
            let moved = fresh.gateway != link.gateway
                || fresh.connection.kind != link.connection.kind
                || fresh.connection.adapter != link.connection.adapter;
            if moved {
                let t = shared.now();
                shared.with(|s| {
                    s.collector.set_connection(fresh.connection.clone());
                    s.targets.set_router(&mut s.collector, fresh.gateway, t);
                });
                sampler.set_wifi(fresh.wifi);
                link = fresh;
            }
        }
    }
}
