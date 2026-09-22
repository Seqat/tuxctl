//! Per-second rates from monotonically increasing kernel counters.

use std::time::Instant;

/// Samples closer together than this give unreliable rates and report none.
const MIN_RATE_INTERVAL_SECS: f64 = 0.001;

/// `N` counters read at the same instant (e.g. RX/TX bytes of one interface).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CounterSample<const N: usize> {
    values: [u64; N],
    at: Instant,
}

impl<const N: usize> CounterSample<N> {
    pub(super) fn new(values: [u64; N], at: Instant) -> Self {
        Self { values, at }
    }

    /// Per-second rate of each counter since `previous`.
    ///
    /// There is no rate without a previous sample or when the samples are less
    /// than 1 ms apart. A counter that went backwards (reset or wrap) reports
    /// 0.0 for this sample. Callers drop their baseline when a read fails.
    pub(super) fn rates_since(&self, previous: Option<&Self>) -> [Option<f64>; N] {
        let Some(previous) = previous else {
            return [None; N];
        };
        let elapsed = self.at.saturating_duration_since(previous.at).as_secs_f64();
        if elapsed < MIN_RATE_INTERVAL_SECS {
            return [None; N];
        }
        std::array::from_fn(|index| {
            Some(
                self.values[index]
                    .checked_sub(previous.values[index])
                    .map_or(0.0, |delta| delta as f64 / elapsed),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn first_sample_has_no_rate() {
        let sample = CounterSample::new([1_000_000, 500_000], Instant::now());

        assert_eq!(sample.rates_since(None), [None, None]);
    }

    #[test]
    fn rates_come_from_counter_and_elapsed_deltas() {
        let t0 = Instant::now();
        let previous = CounterSample::new([1_000_000, 500_000], t0);
        let current = CounterSample::new(
            [1_000_000 + 2_097_152, 500_000 + 1_048_576],
            t0 + Duration::from_secs(2),
        );

        assert_eq!(
            current.rates_since(Some(&previous)),
            [Some(1_048_576.0), Some(524_288.0)]
        );
    }

    #[test]
    fn samples_less_than_a_millisecond_apart_have_no_rate() {
        let t0 = Instant::now();
        let previous = CounterSample::new([100, 200], t0);
        let current = CounterSample::new([200, 300], t0 + Duration::from_micros(999));

        assert_eq!(current.rates_since(Some(&previous)), [None, None]);
    }

    #[test]
    fn a_counter_that_went_backwards_reports_zero_for_that_sample() {
        let t0 = Instant::now();
        let previous = CounterSample::new([10_000, 5_000], t0);
        let current = CounterSample::new([100, 5_500], t0 + Duration::from_secs(1));

        assert_eq!(
            current.rates_since(Some(&previous)),
            [Some(0.0), Some(500.0)]
        );
    }
}
