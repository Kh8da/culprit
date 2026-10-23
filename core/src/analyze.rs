//! Turns a recording into a report: incidents, their causes, and the verdict.

use crate::attribute::{best_cause, Cand, CandStat, Ctx};
use crate::detect::{self, find_lag, find_stutters, FrameStats, Lag, LagSign, PingTrack, Stutter};
use crate::model::{LinkKind, Recording, TargetRole};
use crate::report::*;
use crate::stats::{mean, median, regular_interval};
use std::collections::HashMap;

/// Incidents up to this long before a hotkey press count as the one the player meant.
const MARK_BEFORE_MS: f64 = 6_000.0;
const MARK_AFTER_MS: f64 = 1_000.0;

pub fn analyze(rec: &Recording) -> Report {
    let (stutters, fstats) = find_stutters(rec);
    let tracks = detect::track_all(rec);
    let lags = find_lag(rec, &tracks);
    let ctx = Ctx::new(rec);
    let net = NetTracks::new(&tracks);

    let game_since = rec.game.as_ref().map(|g| g.since).unwrap_or(0.0);
    let last_frame = rec.frames.last().map(|f| f.t);
    let sys = &rec.system;
    let st_eligible: Vec<bool> = sys
        .iter()
        .map(|s| s.foreground && s.t - 1000.0 >= game_since && last_frame.is_some_and(|l| s.t <= l + 1000.0))
        .collect();
    let lag_eligible: Vec<bool> = sys.iter().map(|s| s.t - 1000.0 >= game_since).collect();
    let mark = |windows: &mut dyn Iterator<Item = (f64, f64)>| {
        let mut secs = vec![false; sys.len()];
        for (a, b) in windows {
            for i in ctx.covering(a, b) {
                secs[i] = true;
            }
        }
        secs
    };
    let st_secs = mark(&mut stutters.iter().map(|s| (s.t - 300.0, s.end)));
    let lag_secs = mark(&mut lags.iter().map(|l| (l.t - 1000.0, l.end + 500.0)));

    let cands = ctx.candidates();
    let st_cands: Vec<Cand> = cands.iter().copied().filter(|c| c.for_stutters()).collect();
    let lag_cands: Vec<Cand> = cands.iter().copied().filter(|c| !c.for_stutters()).collect();
    let st_stats = ctx.stats(&st_cands, &st_secs, &st_eligible);
    let lag_stats = ctx.stats(&lag_cands, &lag_secs, &lag_eligible);

    let mut incidents: Vec<Incident> = Vec::new();
    for s in &stutters {
        incidents.push(stutter_incident(&ctx, &st_stats, s));
    }
    for l in &lags {
        incidents.push(lag_incident(&ctx, &lag_stats, &net, rec, l));
    }
    incidents.sort_by(|a, b| a.t.total_cmp(&b.t));
    apply_marks(&ctx, &st_stats, &net, rec, &fstats, &mut incidents);

    let culprits = culprits(&ctx, &st_stats, &lag_stats, rec, &incidents);
    let perf = (fstats.frames > 0).then(|| PerfSummary {
        frames: fstats.frames,
        avg_fps: fstats.avg_fps,
        low1_fps: fstats.low1_fps,
        stutters: stutters.len() as u32,
        stutters_per_min: if fstats.minutes > 0.0 { stutters.len() as f32 / fstats.minutes } else { 0.0 },
        worst_ms: fstats.worst_ms,
    });
    let net_summary = net_summary(rec, &net, lags.len() as u32);
    let (headline, detail, mood) = verdict(rec, &perf, &net_summary, &culprits, &incidents);

    let mut notes = rec.notes.clone();
    if let Some(n) = &net_summary {
        if n.server.is_some() && !n.server_answers_ping {
            notes.push(
                "The game server doesn't answer pings, so its ping was measured at the last stop on the way to it."
                    .into(),
            );
        }
    }
    if !net.router.is_empty() && !net.router.iter().any(|t| t.trustworthy()) {
        notes.push("Your router doesn't answer pings reliably, so Culprit couldn't check the link to it.".into());
    }

    Report {
        version: REPORT_VERSION,
        headline,
        detail,
        mood,
        game: rec.game.as_ref().map(|g| g.name.clone()),
        duration_ms: rec.duration_ms,
        perf,
        net: net_summary,
        culprits,
        incidents,
        notes,
    }
}

/* ---------- wording helpers ---------- */

fn mbit(bytes_per_s: f32) -> String {
    let m = bytes_per_s / 125_000.0;
    if m >= 10.0 {
        format!("{m:.0} Mbit/s")
    } else {
        format!("{m:.1} Mbit/s")
    }
}

fn mbytes(bytes_per_s: f32) -> String {
    format!("{:.0} MB/s", bytes_per_s / 1_000_000.0)
}

fn ms(v: f32) -> String {
    if v >= 1000.0 {
        format!("{:.1} s", v / 1000.0)
    } else {
        format!("{v:.0} ms")
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// "14 of your 20 stutters", "all 6 of your stutters", "your only stutter".
fn share(count: u32, of: u32, kind: IncidentKind) -> String {
    let (one, many) = noun(kind);
    if of == 1 {
        format!("your only {one}")
    } else if count == of {
        format!("all {of} of your {many}")
    } else {
        format!("{count} of your {of} {many}")
    }
}

fn noun(kind: IncidentKind) -> (&'static str, &'static str) {
    match kind {
        IncidentKind::Stutter => ("stutter", "stutters"),
        IncidentKind::Lag => ("lag spike", "lag spikes"),
        IncidentKind::Marked => ("marked moment", "marked moments"),
    }
}

/* ---------- causes ---------- */

fn cand_cause(rec: &Recording, c: Cand) -> Cause {
    let p = |i: usize| (rec.processes[i].key.clone(), rec.processes[i].label.clone());
    match c {
        Cand::AppCpu(i) => {
            let (key, label) = p(i);
            Cause::App { key, label }
        }
        Cand::AppIo(i) => {
            let (key, label) = p(i);
            Cause::AppDisk { key, label }
        }
        Cand::AppNet(i) => {
            let (key, label) = p(i);
            Cause::AppNetwork { key, label }
        }
        Cand::CpuMaxed => Cause::CpuMaxed,
        Cand::CpuSlowed => Cause::CpuSlowed,
        Cand::RamFull => Cause::RamFull,
        Cand::VramFull => Cause::VramFull,
        Cand::GpuThrottled => Cause::GpuThrottled,
    }
}

/// One line of evidence for a cause that was active in `range`, with the numbers.
fn cand_evidence(ctx: &Ctx, c: Cand, range: std::ops::Range<usize>) -> String {
    let rec = ctx.rec;
    let max_of = |f: &dyn Fn(usize) -> Option<f32>| range.clone().filter_map(f).fold(f32::NAN, f32::max);
    match c {
        Cand::AppCpu(p) => {
            let peak = max_of(&|i| ctx.proc_sample(p, i).map(|s| s.cpu));
            let typical = ctx.cpu_typical(p);
            let usual = if typical < 1.0 { "under 1%".to_string() } else { format!("{typical:.0}%") };
            format!("{} jumped to {:.0}% of your CPU (it usually uses {usual}).", rec.processes[p].label, peak)
        }
        Cand::AppIo(p) => {
            let peak = max_of(&|i| ctx.proc_sample(p, i).map(|s| s.io));
            format!("{} was reading and writing {} on disk.", rec.processes[p].label, mbytes(peak))
        }
        Cand::AppNet(p) => {
            let down = max_of(&|i| ctx.proc_sample(p, i).map(|s| s.net_in));
            let up = max_of(&|i| ctx.proc_sample(p, i).map(|s| s.net_out));
            if up * 4.0 > down {
                format!("{} was uploading at {} on your connection.", rec.processes[p].label, mbit(up))
            } else {
                format!("{} was downloading at {} on your connection.", rec.processes[p].label, mbit(down))
            }
        }
        Cand::CpuMaxed => {
            let cpu = max_of(&|i| ctx.sys[i].cpu);
            let game = max_of(&|i| ctx.sys[i].game_cpu.map(|g| ctx.sys[i].cpu.unwrap_or(0.0) - g));
            if game.is_finite() {
                format!("Your CPU was at {cpu:.0}%, and {game:.0}% of that was other programs.")
            } else {
                format!("Your CPU was at {cpu:.0}%.")
            }
        }
        Cand::CpuSlowed => {
            let perf = range.clone().filter_map(|i| ctx.sys[i].cpu_perf).fold(f32::NAN, f32::min);
            format!("Your CPU had dropped to {perf:.0}% of its rated speed.")
        }
        Cand::RamFull => {
            let free = range.clone().filter_map(|i| ctx.sys[i].mem_free_mb).fold(f32::NAN, f32::min);
            let faults = max_of(&|i| ctx.sys[i].hard_faults);
            match (free.is_finite(), faults.is_finite()) {
                (true, true) => format!(
                    "Only {free:.0} MB of RAM was free, and Windows read {faults:.0} pages per second back from disk."
                ),
                (true, false) => format!("Only {free:.0} MB of RAM was free."),
                _ => format!("Windows read {faults:.0} pages per second back from disk because RAM ran short."),
            }
        }
        Cand::VramFull => {
            let used = max_of(&|i| ctx.sys[i].vram_used_mb);
            let total = rec.machine.vram_total_mb.unwrap_or(0.0);
            format!("GPU memory was full: {used:.0} of {total:.0} MB in use.")
        }
        Cand::GpuThrottled => {
            let temp = max_of(&|i| ctx.sys[i].gpu_temp);
            let thermal = range.clone().any(|i| ctx.sys[i].gpu_throttle.is_some_and(|g| g.thermal));
            match (thermal, temp.is_finite()) {
                (true, true) => format!("The GPU was slowing itself down to cool off at {temp:.0}°C."),
                (true, false) => "The GPU was slowing itself down because it was too hot.".into(),
                (false, true) => format!("The GPU was being held back by a power or hardware limit, at {temp:.0}°C."),
                (false, false) => "The GPU was being held back by a power or hardware limit.".into(),
            }
        }
    }
}

fn confidence(st: &CandStat) -> Confidence {
    let lift = st.lift();
    if lift >= 3.0 && st.hits >= 3 && st.hits as f32 >= st.incident_secs as f32 * 0.3 {
        Confidence::High
    } else if lift >= 1.5 {
        Confidence::Medium
    } else {
        Confidence::Low
    }
}

fn stutter_what(s: &Stutter) -> String {
    if s.frames == 1 {
        format!("One frame took {} (normally {})", ms(s.worst_ms), ms(s.normal_ms))
    } else {
        format!("{} slow frames, the worst {} (normally {})", s.frames, ms(s.worst_ms), ms(s.normal_ms))
    }
}

fn stutter_incident(ctx: &Ctx, stats: &HashMap<Cand, CandStat>, s: &Stutter) -> Incident {
    let (a, b) = (s.t - 300.0, s.end);
    let range = ctx.covering(a, b);
    let (cause, confidence, evidence) = match best_cause(ctx, stats, a, b, true) {
        Some((c, st)) => (cand_cause(ctx.rec, c), confidence(&st), vec![cand_evidence(ctx, c, range)]),
        None => game_cause(ctx, s, range),
    };
    Incident {
        t: s.t,
        dur: s.end - s.t,
        kind: IncidentKind::Stutter,
        what: stutter_what(s),
        size_ms: s.worst_ms,
        cause_title: cause.title(),
        cause,
        confidence,
        evidence,
        marked: false,
    }
}

/// When nothing else on the PC stood out, what the game itself was doing.
fn game_cause(ctx: &Ctx, s: &Stutter, range: std::ops::Range<usize>) -> (Cause, Confidence, Vec<String>) {
    let io = range.clone().filter_map(|i| ctx.sys[i].game_io).fold(0.0f32, f32::max);
    if io >= (ctx.game_io_typical * 4.0).max(30_000_000.0) {
        return (
            Cause::GameLoading,
            Confidence::Medium,
            vec![format!("The game was reading {} from disk right then, and nothing else stood out.", mbytes(io))],
        );
    }
    let worst = s.worst_ms;
    match (s.cpu_busy, s.gpu_busy) {
        (Some(cpu), _) if cpu >= worst * 0.6 => (
            Cause::GameCpu,
            Confidence::Medium,
            vec![format!(
                "The game's own CPU work took {} of that {} frame, and nothing else on your PC was unusually busy.",
                ms(cpu),
                ms(worst)
            )],
        ),
        (_, Some(gpu)) if gpu >= worst * 0.6 => (
            Cause::GameGpu,
            Confidence::Medium,
            vec![format!("The GPU spent {} drawing that one frame, and nothing else on your PC was unusually busy.", ms(gpu))],
        ),
        (Some(cpu), Some(gpu)) if cpu < worst * 0.3 && gpu < worst * 0.3 => (
            Cause::Unknown,
            Confidence::Low,
            vec!["The game wasn't busy during the hitch, it was waiting on something. That's usually a driver, the disk or Windows itself.".into()],
        ),
        _ => (Cause::Game, Confidence::Low, vec!["Nothing else on your PC was unusually busy at that moment.".into()]),
    }
}

/* ---------- network ---------- */

pub struct NetTracks<'a> {
    pub router: Vec<&'a PingTrack>,
    pub isp: Vec<&'a PingTrack>,
    pub path: Vec<&'a PingTrack>,
    pub path_is_server: bool,
}

impl<'a> NetTracks<'a> {
    fn new(tracks: &'a [PingTrack]) -> Self {
        let by_role = |role: TargetRole| tracks.iter().filter(|t| t.role == role).collect::<Vec<_>>();
        let path = detect::path_tracks(tracks);
        Self {
            router: by_role(TargetRole::Router),
            isp: by_role(TargetRole::Isp),
            path_is_server: path.first().is_some_and(|t| t.role == TargetRole::Server),
            path,
        }
    }
}

/// What a group of ping tracks saw in `[a, b]`: `Some(text)` if something was wrong.
fn trouble(tracks: &[&PingTrack], a: f64, b: f64, name: &str) -> Option<String> {
    for t in tracks.iter().filter(|t| t.trustworthy()) {
        let bad: Vec<_> = t.bad_between(a, b).collect();
        if bad.is_empty() {
            continue;
        }
        let lost = bad.iter().filter(|(p, _)| p.rtt.is_none()).count() as u32;
        let worst = bad.iter().filter_map(|(p, _)| p.rtt).fold(f32::NAN, f32::max);
        let normal = t.normal.unwrap_or(0.0);
        return Some(if worst.is_finite() && lost > 0 {
            format!(
                "{name} took {} to answer and missed {} (normally {}).",
                ms(worst),
                plural(lost, "ping", "pings"),
                ms(normal)
            )
        } else if worst.is_finite() {
            format!("{name} took {} to answer (normally {}).", ms(worst), ms(normal))
        } else {
            format!("{name} didn't answer {} in a row.", plural(lost, "ping", "pings"))
        });
    }
    None
}

fn fine(tracks: &[&PingTrack], name: &str) -> Option<String> {
    tracks
        .iter()
        .find(|t| t.trustworthy())
        .map(|t| format!("{name} answered normally ({}).", ms(t.normal.unwrap_or(0.0))))
}

fn lag_what(l: &Lag, server_word: &str) -> String {
    let parts: Vec<String> = l
        .signs
        .iter()
        .map(|s| match s {
            LagSign::Spike { rtt, normal } => {
                format!("Ping to the {server_word} jumped to {} (normally {})", ms(*rtt), ms(*normal))
            }
            LagSign::Loss { lost } => format!("{} to the {server_word} got no answer", plural(*lost, "ping", "pings")),
            LagSign::Silence { ms: gap } => format!("No data from the server for {}", ms(*gap)),
        })
        .collect();
    parts.join(", ")
}

fn lag_incident(ctx: &Ctx, stats: &HashMap<Cand, CandStat>, net: &NetTracks, rec: &Recording, l: &Lag) -> Incident {
    let (a, b) = (l.t - 1000.0, l.end + 1000.0);
    let wifi = rec.connection.kind == LinkKind::Wifi;
    let server_word = if net.path_is_server { "server" } else { "server's network" };
    let mut evidence = Vec::new();
    let router_trouble = trouble(&net.router, a, b, if wifi { "Your router (over Wi-Fi)" } else { "Your router" });
    let isp_trouble = trouble(&net.isp, a, b, "Your provider's first hop");

    let (cause, confidence) = if let Some((c, st)) = best_cause(ctx, stats, l.t - 1500.0, l.end + 500.0, false) {
        evidence.push(cand_evidence(ctx, c, ctx.covering(l.t - 1500.0, l.end + 500.0)));
        evidence.extend(router_trouble.clone().or(isp_trouble.clone()));
        (cand_cause(rec, c), confidence(&st))
    } else if let Some(r) = router_trouble {
        evidence.push(r);
        (Cause::HomeNetwork { wifi }, Confidence::High)
    } else if let Some(i) = isp_trouble {
        evidence.push(i);
        evidence.extend(fine(&net.router, "Your router"));
        (Cause::Isp, Confidence::High)
    } else {
        let checked: Vec<String> =
            fine(&net.router, "Your router").into_iter().chain(fine(&net.isp, "Your provider's first hop")).collect();
        let confidence = match checked.len() {
            2 => Confidence::High,
            1 => Confidence::Medium,
            _ => Confidence::Low,
        };
        if checked.is_empty() {
            evidence.push("Culprit couldn't ping your router or provider on this connection to rule them out.".into());
        }
        evidence.extend(checked);
        (Cause::Server, confidence)
    };
    if wifi {
        let range = ctx.covering(a, b);
        let weakest = range.filter_map(|i| ctx.sys[i].wifi_signal).fold(f32::NAN, f32::min);
        if weakest < 50.0 {
            evidence.push(format!("Your Wi-Fi signal was weak ({weakest:.0}%)."));
        }
    }
    Incident {
        t: l.t,
        dur: l.end - l.t,
        kind: IncidentKind::Lag,
        what: lag_what(l, server_word),
        size_ms: l.size_ms(),
        cause_title: cause.title(),
        cause,
        confidence,
        evidence,
        marked: false,
    }
}

/* ---------- hotkey marks ---------- */

fn apply_marks(
    ctx: &Ctx,
    st_stats: &HashMap<Cand, CandStat>,
    net: &NetTracks,
    rec: &Recording,
    fstats: &FrameStats,
    incidents: &mut Vec<Incident>,
) {
    let mut extra = Vec::new();
    for &m in &rec.marks {
        let mut found = false;
        for inc in incidents.iter_mut() {
            if inc.t >= m - MARK_BEFORE_MS && inc.t <= m + MARK_AFTER_MS {
                inc.marked = true;
                found = true;
            }
        }
        if found {
            continue;
        }
        // Nothing crossed the thresholds, so report what was worst in the seconds before.
        let (a, b) = (m - 5_000.0, m);
        let mut evidence = Vec::new();
        let from = rec.frames.partition_point(|f| f.t < a);
        let worst = rec.frames[from..].iter().take_while(|f| f.t <= b).map(|f| f.ms).fold(f32::NAN, f32::max);
        if worst.is_finite() && fstats.avg_fps > 0.0 {
            evidence.push(format!(
                "The slowest frame in the 5 seconds before took {} (you average {:.0} fps).",
                ms(worst),
                fstats.avg_fps
            ));
        }
        let from = rec.pings.partition_point(|p| p.t < a);
        let path_worst = rec.pings[from..]
            .iter()
            .take_while(|p| p.t <= b)
            .filter(|p| net.path.iter().any(|t| t.target == p.target))
            .filter_map(|p| p.rtt)
            .fold(f32::NAN, f32::max);
        if path_worst.is_finite() {
            evidence.push(format!("Ping to the server peaked at {}.", ms(path_worst)));
        }
        let (cause, confidence) = match best_cause(ctx, st_stats, a, b, true) {
            Some((c, st)) => {
                evidence.push(cand_evidence(ctx, c, ctx.covering(a, b)));
                (cand_cause(rec, c), confidence(&st).min(Confidence::Medium))
            }
            None => {
                evidence.push("Nothing on your PC or connection crossed Culprit's limits around then.".into());
                (Cause::Unknown, Confidence::Low)
            }
        };
        extra.push(Incident {
            t: m,
            dur: 0.0,
            kind: IncidentKind::Marked,
            what: "You pressed the hotkey".into(),
            size_ms: if worst.is_finite() { worst } else { 0.0 },
            cause_title: cause.title(),
            cause,
            confidence,
            evidence,
            marked: true,
        });
    }
    incidents.extend(extra);
    incidents.sort_by(|a, b| a.t.total_cmp(&b.t));
}

/* ---------- culprits ---------- */

fn cause_cand(rec: &Recording, cause: &Cause) -> Option<Cand> {
    let idx = |key: &str| rec.processes.iter().position(|p| p.key == key);
    Some(match cause {
        Cause::App { key, .. } => Cand::AppCpu(idx(key)?),
        Cause::AppDisk { key, .. } => Cand::AppIo(idx(key)?),
        Cause::AppNetwork { key, .. } => Cand::AppNet(idx(key)?),
        Cause::CpuMaxed => Cand::CpuMaxed,
        Cause::CpuSlowed => Cand::CpuSlowed,
        Cause::RamFull => Cand::RamFull,
        Cause::VramFull => Cand::VramFull,
        Cause::GpuThrottled => Cand::GpuThrottled,
        _ => return None,
    })
}

fn culprits(
    ctx: &Ctx,
    st_stats: &HashMap<Cand, CandStat>,
    lag_stats: &HashMap<Cand, CandStat>,
    rec: &Recording,
    incidents: &[Incident],
) -> Vec<Culprit> {
    let mut groups: Vec<((IncidentKind, Cause), Vec<&Incident>)> = Vec::new();
    for inc in incidents.iter().filter(|i| i.kind != IncidentKind::Marked) {
        let key = (inc.kind, inc.cause.clone());
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(inc),
            None => groups.push((key, vec![inc])),
        }
    }
    let total = |kind: IncidentKind| incidents.iter().filter(|i| i.kind == kind).count() as u32;
    let mut out: Vec<Culprit> = groups
        .into_iter()
        .map(|((kind, cause), list)| {
            let of = total(kind);
            let count = list.len() as u32;
            let confidence = list.iter().map(|i| i.confidence).max().unwrap_or(Confidence::Low);
            let mut why = match cause_cand(rec, &cause) {
                Some(c) => {
                    let stats = if kind == IncidentKind::Lag { lag_stats } else { st_stats };
                    let during = incidents
                        .iter()
                        .filter(|i| i.kind == kind)
                        .filter(|i| ctx.covering(i.t - 1000.0, i.t + i.dur + 300.0).any(|s| ctx.active(c, s)))
                        .count();
                    let base = stats.get(&c).map(|s| s.base_rate()).unwrap_or(0.0);
                    let otherwise = if base < 0.005 {
                        "and almost never otherwise".to_string()
                    } else {
                        format!("and only {:.0}% of the rest of the time", base * 100.0)
                    };
                    let doing = match &cause {
                        Cause::App { label, .. } => format!("{label} was in a CPU burst"),
                        Cause::AppDisk { label, .. } => format!("{label} was hammering the disk"),
                        Cause::AppNetwork { label, .. } => format!("{label} was using your connection heavily"),
                        Cause::CpuMaxed => "Your CPU was maxed out".into(),
                        Cause::CpuSlowed => "Your CPU was running slow".into(),
                        Cause::RamFull => "RAM was nearly full".into(),
                        Cause::VramFull => "GPU memory was full".into(),
                        _ => "The GPU was throttling".into(),
                    };
                    format!("{doing} during {during} of {of} {}, {otherwise}.", noun(kind).1)
                }
                None => match &cause {
                    Cause::HomeNetwork { .. } => {
                        format!("Your router was slow or dropped pings during {count} of {of} lag spikes, so the trouble was before the internet.")
                    }
                    Cause::Isp => format!(
                        "Your router was fine, but your provider's first hop was slow or dropped pings during {count} of {of} lag spikes."
                    ),
                    Cause::Server => format!(
                        "Everything up to your internet provider was fine during {count} of {of} lag spikes, so the trouble was further away."
                    ),
                    Cause::Unknown => "The game was waiting on something Culprit can't see, such as a driver, the disk or Windows.".into(),
                    _ => format!("Nothing else on your PC was unusually busy during {count} of {of} {}.", noun(kind).1),
                },
            };
            let times: Vec<f64> = list.iter().map(|i| i.t).collect();
            if let Some(every) = regular_interval(&times, 0.12) {
                why.push_str(&format!(" It came back about every {:.0} seconds, which points to something on a timer.", every / 1000.0));
            }
            let mut fix = fix_for(&cause);
            if matches!(cause, Cause::HomeNetwork { wifi: true }) && regular_interval(&times, 0.12).is_some() {
                fix.push_str(" Wi-Fi lag on a timer is usually something scanning for networks in the background, which a cable avoids entirely.");
            }
            Culprit { title: cause.title(), cause, kind, count, of, confidence, why, fix }
        })
        .collect();
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.cause.is_actionable().cmp(&a.cause.is_actionable()))
            .then(b.confidence.cmp(&a.confidence))
    });
    out
}

fn fix_for(cause: &Cause) -> String {
    let key = match cause {
        Cause::App { key, .. } | Cause::AppDisk { key, .. } | Cause::AppNetwork { key, .. } => key.as_str(),
        _ => "",
    };
    let specific = match key {
        "msmpeng.exe" | "mpdefendercoreservice.exe" | "svc:windefend" => Some(
            "A Microsoft Defender scan ran while you played. Windows normally waits for idle time, so if scans keep landing mid-game, give the \"Windows Defender Scheduled Scan\" task a fixed time in Task Scheduler (Microsoft > Windows > Windows Defender).",
        ),
        "tiworker.exe" | "trustedinstaller.exe" | "svc:wuauserv" | "svc:usosvc" | "svc:trustedinstaller" | "mousocoreworker.exe" => Some(
            "Windows Update was working in the background. Pause updates while you play (Settings > Windows Update > Pause updates), or set your active hours so it waits.",
        ),
        "svc:dosvc" | "svc:bits" => Some(
            "Windows was downloading updates. Limit it in Settings > Windows Update > Advanced options > Delivery Optimization > Advanced options, or pause updates while you play.",
        ),
        "searchindexer.exe" | "searchprotocolhost.exe" | "svc:wsearch" => Some(
            "Windows Search was indexing files. Settings > Privacy & security > Searching Windows lets you switch to Classic or leave your game folders out.",
        ),
        "steam.exe" | "steamservice.exe" | "steamwebhelper.exe" => Some(
            "Steam was busy, usually downloading an update. In Steam > Settings > Downloads, turn off \"Allow downloads during gameplay\".",
        ),
        "onedrive.exe" | "dropbox.exe" | "googledrivefs.exe" => Some("Your cloud sync was busy. Pause syncing from its tray icon while you play."),
        "chrome.exe" | "msedge.exe" | "firefox.exe" | "opera.exe" | "opera_gx.exe" | "brave.exe" | "vivaldi.exe" => Some(
            "Your browser was busy. Close tabs that play video or refresh themselves, or close the browser while you play.",
        ),
        "discord.exe" => Some("Discord was busy. Turning off hardware acceleration (Settings > Advanced) often calms it down while a game runs."),
        "obs64.exe" | "obs32.exe" => Some("OBS was competing with the game. Use your GPU's hardware encoder (NVENC, AMF or QuickSync) instead of x264."),
        "epicgameslauncher.exe" | "battle.net.exe" | "eadesktop.exe" | "upc.exe" | "riotclientservices.exe" | "xboxpcapp.exe" => Some(
            "A game launcher was busy, usually downloading an update. Pause downloads in it while you play.",
        ),
        "qbittorrent.exe" | "utorrent.exe" | "bittorrent.exe" => Some("A torrent client was using your connection. Pause it, or cap its upload and download speeds."),
        _ => None,
    };
    if let Some(s) = specific {
        return s.into();
    }
    match cause {
        Cause::App { label, .. } => format!("{label} used a burst of CPU right as the game hitched. Close it while you play, or look in its settings for a background or gaming mode."),
        Cause::AppDisk { label, .. } => format!("{label} kept the disk busy, so the game had to wait its turn. Pause it while you play, or move the game to another drive."),
        Cause::AppNetwork { label, .. } => format!("{label} was using your connection at the same time. Pause it while you play, or set a speed limit in it."),
        Cause::CpuMaxed => "Your CPU had no headroom left. Close what you don't need in the background, or lower the game's CPU-heavy settings (view distance, crowds, physics).".into(),
        Cause::CpuSlowed => "Your CPU slowed itself down, usually because of heat or a power-saving mode. Plug in if it's a laptop, pick Best performance in Settings > System > Power, and check the cooler isn't full of dust.".into(),
        Cause::RamFull => "Your PC ran out of free memory, so Windows swapped to disk. Close browsers and other big apps before you play.".into(),
        Cause::VramFull => "The game filled the GPU's memory. Lower texture quality by one step.".into(),
        Cause::GpuThrottled => "Your GPU held itself back because of heat or power limits. Clean the dust out of its fans and check the case gets fresh air.".into(),
        Cause::GameCpu => "The game itself was busy, often compiling shaders or loading a new area. These usually fade after the first match on a map; a GPU driver update clears old shader caches if they keep coming.".into(),
        Cause::GameGpu => "The game sent the GPU an unusually heavy frame. Turning down the heaviest settings (shadows, ray tracing, volumetrics) smooths these out.".into(),
        Cause::GameLoading => "The game was loading data. Installing it on an SSD makes these shorter.".into(),
        Cause::Game => "Nothing else on your PC stood out, so these are on the game. Updating the game and your GPU driver is the best bet.".into(),
        Cause::Unknown => "Update your GPU driver first, then chipset and storage drivers. Hitches where the game waits on nothing visible are usually driver trouble.".into(),
        Cause::HomeNetwork { wifi: true } => "Your router was slow to answer too, so the trouble is between your PC and the router. Use a cable if you can. If not, move closer or switch to the router's 5 GHz network.".into(),
        Cause::HomeNetwork { wifi: false } => "Your router itself was slow to answer. Restart it, and check whether another device on your network is downloading.".into(),
        Cause::Isp => "Your router was fine but your internet provider's network wasn't. If it keeps happening, restart your modem and show this report to your provider.".into(),
        Cause::Server => "Everything on your side was fine, so the trouble was the game server or the route to it. If the game lets you, try another server region.".into(),
    }
}

/* ---------- summaries ---------- */

fn net_summary(rec: &Recording, net: &NetTracks, lag_spikes: u32) -> Option<NetSummary> {
    if rec.pings.is_empty() && rec.servers.is_empty() && rec.game_traffic.is_empty() {
        return None;
    }
    let on_path: Vec<&crate::model::Ping> =
        rec.pings.iter().filter(|p| net.path.iter().any(|t| t.target == p.target)).collect();
    let path: Vec<f32> = on_path.iter().filter_map(|p| p.rtt).collect();
    // Jitter compares each answer with the one before it to the same target.
    let mut last: HashMap<u32, f32> = HashMap::new();
    let mut steps = Vec::new();
    for p in &on_path {
        if let Some(r) = p.rtt {
            if let Some(prev) = last.insert(p.target, r) {
                steps.push((r - prev).abs());
            }
        }
    }
    let jitter = mean(&steps);
    let sent = on_path.len();
    let lost = on_path.iter().filter(|p| p.rtt.is_none()).count();
    let typical = |tracks: &[&PingTrack]| tracks.iter().find(|t| t.trustworthy()).and_then(|t| t.normal);
    let signal: Vec<f32> = rec.system.iter().filter_map(|s| s.wifi_signal).collect();
    Some(NetSummary {
        link: rec.connection.kind,
        wifi_signal: median(&signal),
        wifi_band_ghz: rec.connection.wifi_band_ghz,
        server: rec.servers.last().map(|s| s.ip.clone()),
        server_answers_ping: net.path_is_server,
        ping_ms: median(&path),
        jitter_ms: jitter,
        loss_pct: (sent > 0).then(|| lost as f32 * 100.0 / sent as f32),
        router_ms: typical(&net.router),
        isp_ms: typical(&net.isp),
        lag_spikes,
    })
}

fn headline_for(c: &Culprit) -> String {
    let s = share(c.count, c.of, c.kind);
    match &c.cause {
        Cause::App { label, .. } | Cause::AppDisk { label, .. } | Cause::AppNetwork { label, .. } => {
            format!("{label} caused {s}.")
        }
        Cause::CpuMaxed => format!("Your CPU ran out of headroom: {s}."),
        Cause::CpuSlowed => format!("Your CPU slowed down mid-game: {s}."),
        Cause::RamFull => format!("Your PC ran out of memory: {s}."),
        Cause::VramFull => format!("Your GPU ran out of memory: {s}."),
        Cause::GpuThrottled => format!("Your GPU throttled itself: {s}."),
        Cause::GameCpu | Cause::GameGpu | Cause::GameLoading | Cause::Game => {
            if c.of == 1 {
                "Your only stutter came from the game itself, not your PC.".into()
            } else if c.count * 2 > c.of {
                "Most stutters came from the game itself, not your PC.".into()
            } else {
                "Some stutters came from the game itself, not your PC.".into()
            }
        }
        Cause::Unknown => format!("No clear cause on your PC for {s}."),
        Cause::HomeNetwork { wifi: true } => format!("Your Wi-Fi caused {s}."),
        Cause::HomeNetwork { wifi: false } => format!("Your home network caused {s}."),
        Cause::Isp => format!("Your internet provider caused {s}."),
        Cause::Server => format!("The game server caused {s}."),
    }
}

fn verdict(
    rec: &Recording,
    perf: &Option<PerfSummary>,
    net: &Option<NetSummary>,
    culprits: &[Culprit],
    incidents: &[Incident],
) -> (String, String, Mood) {
    let game = rec.game.as_ref().map(|g| g.name.as_str()).unwrap_or("The game");
    let minutes = (rec.duration_ms / 60_000.0).max(1.0 / 60.0) as f32;
    let stutters = incidents.iter().filter(|i| i.kind == IncidentKind::Stutter).count() as u32;
    let lag = incidents.iter().filter(|i| i.kind == IncidentKind::Lag).count() as u32;

    if perf.is_none() && net.as_ref().is_none_or(|n| n.server.is_none()) && incidents.is_empty() {
        return (
            "Culprit didn't catch a game.".into(),
            "Keep Culprit running, then start or switch to your game. It locks on to the game in front of you once it starts drawing frames.".into(),
            Mood::Fair,
        );
    }

    let mut facts = Vec::new();
    if let Some(p) = perf {
        facts.push(format!("{game} averaged {:.0} fps, with 1% lows of {:.0}.", p.avg_fps, p.low1_fps));
    }
    if let Some(ping) = net.as_ref().and_then(|n| n.ping_ms) {
        facts.push(format!("Ping to the server was {ping:.0} ms."));
    }
    let facts = facts.join(" ");

    if stutters + lag == 0 {
        return ("No stutters or lag spikes.".into(), facts, Mood::Good);
    }

    let main_kind = if stutters >= lag { IncidentKind::Stutter } else { IncidentKind::Lag };
    let main = culprits.iter().find(|c| c.kind == main_kind);
    let headline = main
        .map(headline_for)
        .unwrap_or_else(|| format!("{} this session.", share(stutters.max(lag), stutters.max(lag), main_kind)));

    let mut detail = Vec::new();
    let counts = [(stutters, IncidentKind::Stutter), (lag, IncidentKind::Lag)]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, k)| plural(*n, noun(*k).0, noun(*k).1))
        .collect::<Vec<_>>()
        .join(" and ");
    detail.push(format!(
        "{counts} in {}.",
        if minutes < 1.5 { "about a minute".into() } else { format!("{minutes:.0} minutes") }
    ));
    // A second cause worth acting on, or the top cause of the other kind.
    if let Some(next) = culprits.iter().skip(1).find(|c| c.kind == main_kind && c.cause.is_actionable() && c.count >= 2)
    {
        detail.push(format!("{} caused {} more.", next.title, next.count));
    }
    let other_kind = if main_kind == IncidentKind::Stutter { IncidentKind::Lag } else { IncidentKind::Stutter };
    if let Some(other) = culprits.iter().find(|c| c.kind == other_kind) {
        let (one, many) = noun(other_kind);
        detail.push(if other.of == 1 {
            format!("The {one} came from {}.", cause_phrase(&other.cause))
        } else {
            format!("{} of the {} {many} came from {}.", other.count, other.of, cause_phrase(&other.cause))
        });
    }
    if !facts.is_empty() {
        detail.push(facts);
    }

    let per_min_st = stutters as f32 / minutes;
    let per_min_lag = lag as f32 / minutes;
    let mood = if per_min_st >= 1.0 || per_min_lag >= 0.5 { Mood::Bad } else { Mood::Fair };
    (headline, detail.join(" "), mood)
}

/// The cause as it reads in the middle of a sentence: "came from {phrase}".
fn cause_phrase(cause: &Cause) -> String {
    match cause {
        Cause::App { label, .. } | Cause::AppDisk { label, .. } | Cause::AppNetwork { label, .. } => label.clone(),
        Cause::CpuMaxed => "your CPU maxing out".into(),
        Cause::CpuSlowed => "your CPU slowing down".into(),
        Cause::RamFull => "running out of RAM".into(),
        Cause::VramFull => "the GPU running out of memory".into(),
        Cause::GpuThrottled => "the GPU throttling itself".into(),
        Cause::GameCpu | Cause::GameGpu | Cause::GameLoading | Cause::Game => "the game itself".into(),
        Cause::HomeNetwork { wifi: true } => "your Wi-Fi".into(),
        Cause::HomeNetwork { wifi: false } => "your home network".into(),
        Cause::Isp => "your internet provider".into(),
        Cause::Server => "the game server".into(),
        Cause::Unknown => "nothing Culprit could see".into(),
    }
}
