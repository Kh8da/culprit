//! Small statistics helpers. Inputs are never assumed sorted and NaNs are ignored.

/// Median of the finite values, or `None` if there are none.
pub fn median(values: &[f32]) -> Option<f32> {
    percentile(values, 50.0)
}

/// Linear-interpolated percentile (0 to 100) of the finite values. Runs in linear time,
/// so it is safe on millions of frame times.
pub fn percentile(values: &[f32], p: f32) -> Option<f32> {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    let rank = (p.clamp(0.0, 100.0) / 100.0) * (v.len() - 1) as f32;
    let lo = rank.floor() as usize;
    let frac = rank - lo as f32;
    let (_, &mut low, above) = v.select_nth_unstable_by(lo, |a, b| a.total_cmp(b));
    if frac == 0.0 || above.is_empty() {
        return Some(low);
    }
    let high = above.iter().copied().fold(f32::INFINITY, f32::min);
    Some(low + (high - low) * frac)
}

pub fn mean(values: &[f32]) -> Option<f32> {
    let (sum, n) = values.iter().filter(|x| x.is_finite()).fold((0.0f64, 0usize), |(s, n), &x| (s + x as f64, n + 1));
    (n > 0).then(|| (sum / n as f64) as f32)
}

/// Median of a sliding window, kept sorted so each step is a binary search and a shift.
pub struct RollingMedian {
    cap: usize,
    order: std::collections::VecDeque<f32>,
    sorted: Vec<f32>,
}

impl RollingMedian {
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), order: Default::default(), sorted: Vec::with_capacity(cap) }
    }

    pub fn push(&mut self, x: f32) {
        if !x.is_finite() {
            return;
        }
        if self.order.len() == self.cap {
            let old = self.order.pop_front().unwrap();
            let i = self.sorted.partition_point(|v| v.total_cmp(&old).is_lt());
            self.sorted.remove(i);
        }
        let i = self.sorted.partition_point(|v| v.total_cmp(&x).is_lt());
        self.sorted.insert(i, x);
        self.order.push_back(x);
    }

    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    pub fn median(&self) -> Option<f32> {
        let n = self.sorted.len();
        match n {
            0 => None,
            _ if n % 2 == 1 => Some(self.sorted[n / 2]),
            _ => Some((self.sorted[n / 2 - 1] + self.sorted[n / 2]) / 2.0),
        }
    }
}

/// How regular a list of event times is. Returns the typical gap when the gaps between
/// consecutive events vary by less than `tolerance` (a fraction of the gap), using the
/// median gap so one missed or extra event doesn't break it.
pub fn regular_interval(times: &[f64], tolerance: f64) -> Option<f64> {
    if times.len() < 4 {
        return None;
    }
    let gaps: Vec<f32> = times.windows(2).map(|w| (w[1] - w[0]) as f32).collect();
    let typical = median(&gaps)? as f64;
    if typical < 5_000.0 {
        return None;
    }
    // A gap counts if it is the typical gap, or a whole multiple of it (a skipped event).
    let fits = gaps
        .iter()
        .filter(|&&g| {
            let k = (g as f64 / typical).round().max(1.0);
            ((g as f64 - k * typical) / typical).abs() <= tolerance
        })
        .count();
    (fits as f64 >= gaps.len() as f64 * 0.8).then_some(typical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0, f32::NAN];
        assert_eq!(median(&v), Some(3.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&v, 100.0), Some(5.0));
        assert_eq!(percentile(&v, 25.0), Some(2.0));
        assert_eq!(median(&[]), None);
        assert_eq!(mean(&[1.0, 2.0, 6.0]), Some(3.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 50.0), Some(2.5));
        assert_eq!(percentile(&[7.0], 99.0), Some(7.0));
    }

    #[test]
    fn rolling_median_matches_full_sort() {
        let data: Vec<f32> = (0..500).map(|i| ((i * 7919) % 101) as f32).collect();
        let mut rm = RollingMedian::new(31);
        for (i, &x) in data.iter().enumerate() {
            rm.push(x);
            let start = (i + 1).saturating_sub(31);
            assert_eq!(rm.median(), median(&data[start..=i]), "at {i}");
        }
    }

    #[test]
    fn regular_intervals() {
        let every_minute: Vec<f64> = (0..8).map(|i| i as f64 * 60_000.0 + (i % 3) as f64 * 400.0).collect();
        let found = regular_interval(&every_minute, 0.1).unwrap();
        assert!((found - 60_000.0).abs() < 1_000.0);

        // One event missed still reads as regular.
        let mut skipped = every_minute.clone();
        skipped.remove(4);
        assert!(regular_interval(&skipped, 0.1).is_some());

        let random = [0.0, 13_000.0, 15_000.0, 41_000.0, 90_000.0, 97_000.0];
        assert_eq!(regular_interval(&random, 0.1), None);
    }
}
