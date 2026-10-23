//! The recording's clock. Windows stamps frames (PresentMon), packets (the network trace)
//! and the recorder's own samples with the same performance counter, so all of them are
//! turned into milliseconds since the start with one origin.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    /// Counter value when the recording started.
    pub origin: u64,
    /// Counter ticks per second.
    pub freq: u64,
}

impl Clock {
    /// Milliseconds since the recording started; negative for anything earlier.
    pub fn ms(&self, counter: u64) -> f64 {
        (counter as i128 - self.origin as i128) as f64 * 1000.0 / self.freq.max(1) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_counter_values() {
        let c = Clock { origin: 50_000_000, freq: 10_000_000 };
        assert_eq!(c.ms(50_000_000), 0.0);
        assert_eq!(c.ms(60_000_000), 1000.0);
        assert_eq!(c.ms(49_990_000), -1.0);
        assert!(c.ms(u64::MAX) > 1e12);
    }
}
