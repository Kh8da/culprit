//! Deciding what caused each incident.
//!
//! Every second of the session, each possible cause is either active (a program in a CPU
//! burst, the CPU maxed out, the GPU throttling...) or not. A cause is only blamed when it
//! is active far more often during incidents than the rest of the time (its "lift"), so a
//! program that is always a little busy doesn't get blamed for stutters it didn't cause.

use crate::model::{ProcSample, ProcessTrack, Recording, SystemSample};
use crate::stats::{median, percentile};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cand {
    AppCpu(usize),
    AppIo(usize),
    AppNet(usize),
    CpuMaxed,
    CpuSlowed,
    RamFull,
    VramFull,
    GpuThrottled,
}

impl Cand {
    /// How much to trust this kind of signal relative to the others when several fire at once.
    fn weight(self) -> f32 {
        match self {
            Cand::RamFull | Cand::VramFull | Cand::GpuThrottled => 1.3,
            Cand::CpuSlowed => 1.2,
            Cand::AppCpu(_) | Cand::AppIo(_) | Cand::AppNet(_) => 1.0,
            Cand::CpuMaxed => 0.9,
        }
    }

    pub fn for_stutters(self) -> bool {
        !matches!(self, Cand::AppNet(_))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CandStat {
    /// Incident seconds where the cause was active, out of all incident seconds.
    pub hits: u32,
    pub incident_secs: u32,
    /// Other seconds where it was active, out of all other seconds.
    pub base_hits: u32,
    pub base_secs: u32,
}

impl CandStat {
    pub fn lift(&self) -> f32 {
        let during = (self.hits as f32 + 0.5) / (self.incident_secs as f32 + 1.0);
        let otherwise = (self.base_hits as f32 + 0.5) / (self.base_secs as f32 + 1.0);
        during / otherwise
    }

    pub fn base_rate(&self) -> f32 {
        if self.base_secs == 0 {
            0.0
        } else {
            self.base_hits as f32 / self.base_secs as f32
        }
    }
}

pub struct Ctx<'a> {
    pub rec: &'a Recording,
    pub sys: &'a [SystemSample],
    /// Per process track: (system sample index, process sample index), sorted. Processes
    /// are only recorded for seconds they were busy, so this stays short.
    at: Vec<Vec<(usize, usize)>>,
    cpu_typical: Vec<f32>,
    io_typical: Vec<f32>,
    cpu_perf_ref: Option<f32>,
    pub game_io_typical: f32,
    pub bytes_per_mbit: f32,
}

/// Pairs each process sample with the system sample stamped closest to it, walking both
/// time-ordered lists once.
fn align(sys: &[SystemSample], samples: &[ProcSample]) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(samples.len());
    let mut i = 0;
    for (j, s) in samples.iter().enumerate() {
        while i + 1 < sys.len() && (sys[i + 1].t - s.t).abs() <= (sys[i].t - s.t).abs() {
            i += 1;
        }
        if sys.get(i).is_some_and(|x| (x.t - s.t).abs() < 500.0) && out.last().is_none_or(|&(k, _)| k != i) {
            out.push((i, j));
        }
    }
    out
}

/// Median of `values` padded with zeros up to `total`, without building the padded list.
fn median_with_zeros(values: &[f32], total: usize) -> f32 {
    let total = total.max(values.len());
    if total == 0 {
        return 0.0;
    }
    let zeros = total - values.len();
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    let nth = |k: usize| if k < zeros { 0.0 } else { v.get(k - zeros).copied().unwrap_or(0.0) };
    if total % 2 == 1 {
        nth(total / 2)
    } else {
        (nth(total / 2 - 1) + nth(total / 2)) / 2.0
    }
}

impl<'a> Ctx<'a> {
    pub fn new(rec: &'a Recording) -> Self {
        let sys = &rec.system[..];
        let at: Vec<Vec<(usize, usize)>> = rec
            .processes
            .iter()
            .map(|p| {
                if p.samples.windows(2).all(|w| w[0].t <= w[1].t) {
                    align(sys, &p.samples)
                } else {
                    let mut sorted: Vec<(usize, &ProcSample)> = p.samples.iter().enumerate().collect();
                    sorted.sort_by(|a, b| a.1.t.total_cmp(&b.1.t));
                    let only: Vec<ProcSample> = sorted.iter().map(|x| *x.1).collect();
                    align(sys, &only).into_iter().map(|(i, k)| (i, sorted[k].0)).collect()
                }
            })
            .collect();
        // Seconds a process wasn't recorded it was idle, so typical values count them as 0.
        let typical = |p: &ProcessTrack, f: fn(&crate::model::ProcSample) -> f32| {
            let v: Vec<f32> = p.samples.iter().map(f).collect();
            median_with_zeros(&v, sys.len())
        };
        let cpu_typical = rec.processes.iter().map(|p| typical(p, |s| s.cpu)).collect();
        let io_typical = rec.processes.iter().map(|p| typical(p, |s| s.io)).collect();
        let perf: Vec<f32> = sys.iter().filter_map(|s| s.cpu_perf).collect();
        let cpu_perf_ref = percentile(&perf, 75.0).filter(|&p| p >= 60.0);
        let game_io: Vec<f32> = sys.iter().filter_map(|s| s.game_io).collect();
        Self {
            rec,
            sys,
            at,
            cpu_typical,
            io_typical,
            cpu_perf_ref,
            game_io_typical: median(&game_io).unwrap_or(0.0),
            bytes_per_mbit: 125_000.0,
        }
    }

    pub fn proc_sample(&self, p: usize, sys_index: usize) -> Option<&crate::model::ProcSample> {
        let at = &self.at[p];
        let k = at.binary_search_by_key(&sys_index, |x| x.0).ok()?;
        self.rec.processes[p].samples.get(at[k].1)
    }

    pub fn candidates(&self) -> Vec<Cand> {
        let mut c = vec![Cand::CpuMaxed, Cand::CpuSlowed, Cand::RamFull, Cand::VramFull, Cand::GpuThrottled];
        for p in 0..self.rec.processes.len() {
            c.extend([Cand::AppCpu(p), Cand::AppIo(p), Cand::AppNet(p)]);
        }
        c
    }

    pub fn cpu_typical(&self, p: usize) -> f32 {
        self.cpu_typical[p]
    }

    pub fn io_typical(&self, p: usize) -> f32 {
        self.io_typical[p]
    }

    /// Whether a process sample counts as a burst for a per-process candidate.
    fn proc_active(&self, c: Cand, x: &ProcSample) -> bool {
        match c {
            Cand::AppCpu(p) => x.cpu >= (self.cpu_typical[p] + 5.0).max(8.0),
            Cand::AppIo(p) => x.io >= (self.io_typical[p] * 3.0).max(20_000_000.0),
            Cand::AppNet(_) => x.net_in >= 12.0 * self.bytes_per_mbit || x.net_out >= 3.0 * self.bytes_per_mbit,
            _ => false,
        }
    }

    pub fn active(&self, c: Cand, i: usize) -> bool {
        let s = &self.sys[i];
        match c {
            Cand::AppCpu(p) | Cand::AppIo(p) | Cand::AppNet(p) => {
                self.proc_sample(p, i).is_some_and(|x| self.proc_active(c, x))
            }
            Cand::CpuMaxed => match s.cpu {
                Some(cpu) => cpu >= 95.0 && cpu - s.game_cpu.unwrap_or(0.0) >= 20.0,
                None => false,
            },
            Cand::CpuSlowed => match (s.cpu_perf, self.cpu_perf_ref) {
                (Some(perf), Some(reference)) => perf < reference * 0.75,
                _ => false,
            },
            Cand::RamFull => {
                let total = self.rec.machine.mem_total_mb.unwrap_or(0.0);
                s.hard_faults.is_some_and(|f| f >= 250.0)
                    || (total > 2_000.0 && s.mem_free_mb.is_some_and(|m| m < 400.0))
            }
            Cand::VramFull => match (s.vram_used_mb, self.rec.machine.vram_total_mb) {
                (Some(used), Some(total)) if total > 0.0 => used / total >= 0.97,
                _ => false,
            },
            Cand::GpuThrottled => s.gpu_throttle.is_some_and(|g| g.any()),
        }
    }

    /// System samples whose second overlaps `[a, b]`.
    pub fn covering(&self, a: f64, b: f64) -> std::ops::Range<usize> {
        let start = self.sys.partition_point(|s| s.t <= a);
        let end = self.sys.partition_point(|s| s.t - 1000.0 < b);
        start..end.max(start)
    }

    /// Lift statistics for each candidate, given which seconds are "incident" seconds and
    /// which seconds count at all.
    pub fn stats(&self, cands: &[Cand], incident: &[bool], eligible: &[bool]) -> HashMap<Cand, CandStat> {
        let mut out: HashMap<Cand, CandStat> = HashMap::new();
        let n = self.sys.len().min(incident.len()).min(eligible.len());
        let incident_secs = (0..n).filter(|&i| eligible[i] && incident[i]).count() as u32;
        let base_secs = (0..n).filter(|&i| eligible[i] && !incident[i]).count() as u32;
        for &c in cands {
            let mut st = CandStat { incident_secs, base_secs, ..Default::default() };
            let mut tally = |i: usize, on: bool| {
                if on && i < n && eligible[i] {
                    if incident[i] {
                        st.hits += 1;
                    } else {
                        st.base_hits += 1;
                    }
                }
            };
            match c {
                // A process can only be active in seconds it was recorded, so only visit those.
                Cand::AppCpu(p) | Cand::AppIo(p) | Cand::AppNet(p) => {
                    let samples = &self.rec.processes[p].samples;
                    for &(i, j) in &self.at[p] {
                        tally(i, self.proc_active(c, &samples[j]));
                    }
                }
                _ => (0..n).for_each(|i| tally(i, self.active(c, i))),
            }
            if st.hits > 0 {
                out.insert(c, st);
            }
        }
        out
    }
}

/// The best explanation among the causes active during `[a, b]`, if any stands out.
pub fn best_cause(
    ctx: &Ctx,
    stats: &HashMap<Cand, CandStat>,
    a: f64,
    b: f64,
    stutter: bool,
) -> Option<(Cand, CandStat)> {
    let range = ctx.covering(a, b);
    let mut best: Option<(Cand, CandStat, f32)> = None;
    for (&c, st) in stats {
        // Stutters are explained by anything but network use; lag only by network use.
        if stutter != c.for_stutters() {
            continue;
        }
        if !range.clone().any(|i| ctx.active(c, i)) {
            continue;
        }
        let lift = st.lift();
        // Few incidents means weak statistics: then only blame causes that are rarely active.
        let enough =
            if st.incident_secs >= 3 { st.hits >= 2 && lift >= 1.5 } else { st.base_rate() <= 0.25 && lift >= 1.5 };
        if !enough {
            continue;
        }
        let score = lift * c.weight();
        if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
            best = Some((c, *st, score));
        }
    }
    best.map(|(c, st, _)| (c, st))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::model::{ProcSample, ProcessTrack};

    fn rec_with(proc_cpu: impl Fn(usize) -> f32, n: usize) -> Recording {
        let mut rec = Recording::default();
        rec.system = (1..=n)
            .map(|s| SystemSample { t: s as f64 * 1000.0, cpu: Some(40.0), foreground: true, ..Default::default() })
            .collect();
        rec.processes.push(ProcessTrack {
            key: "msmpeng.exe".into(),
            label: "Microsoft Defender".into(),
            kind: Default::default(),
            samples: (1..=n)
                .filter(|&s| proc_cpu(s) > 0.5)
                .map(|s| ProcSample { t: s as f64 * 1000.0, cpu: proc_cpu(s), ..Default::default() })
                .collect(),
        });
        rec
    }

    #[test]
    fn zero_padded_median() {
        assert_eq!(median_with_zeros(&[5.0, 9.0], 10), 0.0);
        assert_eq!(median_with_zeros(&[5.0, 9.0, 7.0], 4), 6.0);
        assert_eq!(median_with_zeros(&[5.0, 9.0, 7.0], 3), 7.0);
        assert_eq!(median_with_zeros(&[], 0), 0.0);
    }

    #[test]
    fn process_samples_find_their_second() {
        let mut rec = rec_with(|s| if s == 3 { 20.0 } else { 0.0 }, 5);
        // Ticks drift in real recordings; a sample still lands on its own second.
        for (i, s) in rec.system.iter_mut().enumerate() {
            s.t += i as f64 * 37.0;
        }
        rec.processes[0].samples[0].t = rec.system[2].t;
        let ctx = Ctx::new(&rec);
        assert_eq!(ctx.proc_sample(0, 2).map(|s| s.cpu), Some(20.0));
        assert!(ctx.proc_sample(0, 1).is_none());
    }

    #[test]
    fn covering_seconds() {
        let rec = rec_with(|_| 0.0, 10);
        let ctx = Ctx::new(&rec);
        // Second stamped 3000 covers (2000, 3000].
        assert_eq!(ctx.covering(2_500.0, 2_600.0), 2..3);
        assert_eq!(ctx.covering(2_900.0, 3_100.0), 2..4);
    }

    #[test]
    fn bursty_app_gets_lift_steady_app_does_not() {
        // Bursts every 10 seconds, incidents exactly on the bursts.
        let rec = rec_with(|s| if s % 10 == 0 { 40.0 } else { 1.0 }, 100);
        let ctx = Ctx::new(&rec);
        let incident: Vec<bool> = (1..=100).map(|s| s % 10 == 0).collect();
        let eligible = vec![true; 100];
        let stats = ctx.stats(&ctx.candidates(), &incident, &eligible);
        let st = stats[&Cand::AppCpu(0)];
        assert_eq!((st.hits, st.incident_secs), (10, 10));
        assert!(st.lift() > 5.0);
        let best = best_cause(&ctx, &stats, 9_500.0, 9_600.0, true).unwrap();
        assert_eq!(best.0, Cand::AppCpu(0));

        // Always busy: never stands out.
        let rec = rec_with(|_| 30.0, 100);
        let ctx = Ctx::new(&rec);
        let stats = ctx.stats(&ctx.candidates(), &incident, &eligible);
        assert!(best_cause(&ctx, &stats, 9_500.0, 9_600.0, true).is_none());
    }
}
