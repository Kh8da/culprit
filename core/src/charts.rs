//! Compact series for drawing a session: small enough to save, detailed enough to see hitches.

use crate::model::{Recording, TargetRole};
use serde::{Deserialize, Serialize};

/// Never draw more points than this per line.
const MAX_POINTS: f64 = 3_000.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Charts {
    /// Width of one point in milliseconds.
    pub step_ms: f64,
    /// Average and worst frame time per step; `None` where the game drew nothing.
    pub frame_avg: Vec<Option<f32>>,
    pub frame_max: Vec<Option<f32>>,
    pub pings: Vec<PingLine>,
    /// CPU use per second, for context.
    pub cpu: Vec<Option<f32>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PingLine {
    pub role: Option<TargetRole>,
    pub ip: String,
    /// Worst round trip per step; `None` where nothing was sent; `-1` where a ping was lost.
    pub rtt: Vec<Option<f32>>,
}

pub fn build(rec: &Recording) -> Charts {
    let duration = rec.duration_ms.max(rec.frames.last().map(|f| f.t).unwrap_or(0.0)).max(1.0);
    let step = ((duration / MAX_POINTS / 100.0).ceil() * 100.0).max(100.0);
    let n = (duration / step).ceil() as usize + 1;
    let slot = |t: f64| ((t / step) as usize).min(n - 1);

    let mut sum = vec![0.0f64; n];
    let mut count = vec![0u32; n];
    let mut max = vec![None::<f32>; n];
    for f in &rec.frames {
        let i = slot(f.t);
        sum[i] += f.ms as f64;
        count[i] += 1;
        max[i] = Some(max[i].map_or(f.ms, |m: f32| m.max(f.ms)));
    }
    let frame_avg = (0..n).map(|i| (count[i] > 0).then(|| (sum[i] / count[i] as f64) as f32)).collect();

    let pings = rec
        .targets
        .iter()
        .map(|t| {
            let mut rtt = vec![None::<f32>; n];
            for p in rec.pings.iter().filter(|p| p.target == t.id) {
                let i = slot(p.t);
                let v = p.rtt.unwrap_or(-1.0);
                rtt[i] = Some(match rtt[i] {
                    // A loss anywhere in the step wins, then the slowest answer.
                    Some(prev) if prev < 0.0 || v < 0.0 => -1.0,
                    Some(prev) => prev.max(v),
                    None => v,
                });
            }
            PingLine { role: Some(t.role), ip: t.ip.clone(), rtt }
        })
        .collect();

    let mut cpu = vec![None::<f32>; n];
    for s in &rec.system {
        if let Some(v) = s.cpu {
            let i = slot((s.t - 500.0).max(0.0));
            cpu[i] = Some(cpu[i].map_or(v, |c: f32| c.max(v)));
        }
    }

    Charts { step_ms: step, frame_avg, frame_max: max, pings, cpu }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, Ping, Target};

    #[test]
    fn bins_frames_and_pings() {
        let mut rec = Recording { duration_ms: 1_000.0, ..Default::default() };
        rec.frames = vec![
            Frame { t: 10.0, ms: 10.0, ..Default::default() },
            Frame { t: 50.0, ms: 30.0, ..Default::default() },
            Frame { t: 150.0, ms: 16.0, ..Default::default() },
        ];
        rec.targets.push(Target {
            id: 3,
            role: TargetRole::Router,
            ip: "192.168.1.1".into(),
            host: None,
            since: 0.0,
            until: None,
        });
        rec.pings = vec![
            Ping { target: 3, t: 0.0, rtt: Some(2.0) },
            Ping { target: 3, t: 60.0, rtt: Some(9.0) },
            Ping { target: 3, t: 250.0, rtt: None },
        ];
        let c = build(&rec);
        assert_eq!(c.step_ms, 100.0);
        assert_eq!(c.frame_avg[0], Some(20.0));
        assert_eq!(c.frame_max[0], Some(30.0));
        assert_eq!(c.frame_max[1], Some(16.0));
        assert_eq!(c.frame_max[2], None);
        assert_eq!(c.pings[0].rtt[0], Some(9.0));
        assert_eq!(c.pings[0].rtt[2], Some(-1.0));
    }

    #[test]
    fn long_sessions_stay_small() {
        let rec = Recording { duration_ms: 3.0 * 3_600_000.0, ..Default::default() };
        let c = build(&rec);
        assert!(c.frame_avg.len() <= 3_002, "{}", c.frame_avg.len());
    }
}
