//! The engine's histogram of callback times: how long each period's work took, wall time, so that
//! the [floors](https://github.com/HoneySlatt/Magnolia/blob/main/docs/ROADMAP.md#floors) on the
//! worst callback and its 99.9th percentile can be checked on a running engine.
//!
//! The real-time thread records one time per callback, with relaxed atomic additions: no lock, no
//! allocation. Any other thread reads a [`Snapshot`] at any time. The buckets are log-linear:
//! times under 16 ns each have a bucket, and each power of two above is cut into 16 buckets, so a
//! bucket's upper bound is within 6.25 % of any time it holds. The exact maximum is kept apart.

// The processor records into the histogram in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Buckets per power of two of nanoseconds, and the times below which each nanosecond has its own.
const SUB_BUCKETS: u64 = 16;
/// log2 of [`SUB_BUCKETS`].
const SUB_BUCKET_BITS: u32 = 4;
/// The largest power of two of nanoseconds the buckets cover: 2^35 ns is about 34 s, far beyond any
/// period. Longer times land in the last bucket; the maximum still holds them exactly.
const LAST_POWER: u32 = 35;
/// Buckets for 0 to 15 ns, then 16 per power of two from 2^4 to 2^35.
const BUCKETS: usize =
    (SUB_BUCKETS + (LAST_POWER - SUB_BUCKET_BITS + 1) as u64 * SUB_BUCKETS) as usize;

/// The histogram, shared between the real-time thread that records and the threads that read.
#[derive(Debug)]
pub struct CallbackTimes {
    buckets: [AtomicU64; BUCKETS],
    maximum_nanoseconds: AtomicU64,
}

impl Default for CallbackTimes {
    fn default() -> Self {
        Self::new()
    }
}

impl CallbackTimes {
    /// An empty histogram.
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            maximum_nanoseconds: AtomicU64::new(0),
        }
    }

    /// Writes zero into every bucket through `black_box`, so that every page of the histogram is
    /// written before the real-time thread first records into it.
    pub(crate) fn write_every_page(&self) {
        let zero = std::hint::black_box(0);
        for bucket in &self.buckets {
            bucket.store(zero, Ordering::Relaxed);
        }
        self.maximum_nanoseconds.store(zero, Ordering::Relaxed);
    }

    /// Records one callback's time. Real-time safe: two relaxed atomic operations.
    pub fn record(&self, time: Duration) {
        let nanoseconds = u64::try_from(time.as_nanos()).unwrap_or(u64::MAX);
        // `bucket_of` never exceeds the last bucket; `get` keeps a panic off the real-time path.
        if let Some(bucket) = self.buckets.get(bucket_of(nanoseconds)) {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
        self.maximum_nanoseconds
            .fetch_max(nanoseconds, Ordering::Relaxed);
    }

    /// The counts recorded so far. Each bucket is read on its own, so a snapshot taken while the
    /// real-time thread records may miss the callback being recorded, never more.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            buckets: self
                .buckets
                .each_ref()
                .map(|bucket| bucket.load(Ordering::Relaxed)),
            maximum_nanoseconds: self.maximum_nanoseconds.load(Ordering::Relaxed),
        }
    }
}

/// The histogram as read at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    buckets: [u64; BUCKETS],
    maximum_nanoseconds: u64,
}

impl Snapshot {
    /// How many callbacks were recorded.
    pub fn count(&self) -> u64 {
        self.buckets.iter().sum()
    }

    /// The longest callback recorded, exactly; zero if none was.
    pub fn maximum(&self) -> Duration {
        Duration::from_nanos(self.maximum_nanoseconds)
    }

    /// A time that at least `numerator / denominator` of the callbacks did not exceed:
    /// `quantile(999, 1000)` is the 99.9th percentile. It is the upper bound of the bucket that
    /// holds that rank, never above the maximum, so within 6.25 % above the true quantile. `None`
    /// if nothing was recorded, or if the fraction is not between 0 and 1.
    pub fn quantile(&self, numerator: u64, denominator: u64) -> Option<Duration> {
        let count = self.count();
        if count == 0 || denominator == 0 || numerator > denominator {
            return None;
        }
        // The rank of the callback at that fraction, counted from 1, by integers so that 99.9 %
        // of 1,000 is exactly the 999th.
        // Two numbers below 2^64 multiply below 2^128: the product never saturates.
        let rank = (u128::from(numerator).saturating_mul(u128::from(count)))
            .div_ceil(u128::from(denominator));
        // The rank is at most the count, so it fits.
        let rank = u64::try_from(rank).unwrap_or(count).max(1);
        let mut seen: u64 = 0;
        for (index, &bucket_count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(bucket_count);
            if seen >= rank {
                let bound = upper_bound_of(index).min(self.maximum_nanoseconds);
                return Some(Duration::from_nanos(bound));
            }
        }
        // The buckets sum to the count, which the rank does not exceed: the loop has returned.
        Some(self.maximum())
    }

    /// The share of callbacks that took longer than `limit`, between 0 and 1, counting a bucket
    /// that straddles the limit as over it. Zero if nothing was recorded.
    pub fn share_over(&self, limit: Duration) -> f64 {
        let count = self.count();
        if count == 0 {
            return 0.0;
        }
        let limit = u64::try_from(limit.as_nanos()).unwrap_or(u64::MAX);
        let over: u64 = self
            .buckets
            .iter()
            .enumerate()
            .filter(|&(index, _)| upper_bound_of(index) > limit)
            .map(|(_, &bucket_count)| bucket_count)
            .sum();
        over as f64 / count as f64
    }
}

/// The bucket that holds a time in nanoseconds.
fn bucket_of(nanoseconds: u64) -> usize {
    if nanoseconds < SUB_BUCKETS {
        return nanoseconds as usize;
    }
    // From here on, 16 nanoseconds or more: the power is at least `SUB_BUCKET_BITS`, so the
    // subtractions below never saturate, and the sum stays below `BUCKETS`.
    let power = nanoseconds.checked_ilog2().unwrap_or(0).min(LAST_POWER);
    if power == LAST_POWER && nanoseconds >> LAST_POWER > 1 {
        return BUCKETS - 1;
    }
    let shift = power.saturating_sub(SUB_BUCKET_BITS);
    let sub_bucket = (nanoseconds >> shift) & (SUB_BUCKETS - 1);
    let power_buckets = u64::from(shift).saturating_mul(SUB_BUCKETS);
    SUB_BUCKETS
        .saturating_add(power_buckets)
        .saturating_add(sub_bucket) as usize
}

/// The largest time in nanoseconds a bucket holds.
fn upper_bound_of(index: usize) -> u64 {
    let index = index as u64;
    if index < SUB_BUCKETS {
        return index;
    }
    if index as usize == BUCKETS - 1 {
        return u64::MAX;
    }
    // From here on, 16 to `BUCKETS - 2`: no step below saturates.
    let above_linear = index.saturating_sub(SUB_BUCKETS);
    let shift = above_linear / SUB_BUCKETS;
    let sub_bucket = above_linear % SUB_BUCKETS;
    let power = shift.saturating_add(u64::from(SUB_BUCKET_BITS));
    let width = 1u64 << shift;
    (1u64 << power)
        .saturating_add(sub_bucket.saturating_add(1).saturating_mul(width))
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_time_lies_within_its_buckets_bounds() {
        let mut times: Vec<u64> = (0..5_000).collect();
        for power in 4..63 {
            let base = 1u64 << power;
            times.extend([base - 1, base, base + 1, base + base / 2, 2 * base - 1]);
        }
        times.push(u64::MAX);
        for time in times {
            let bucket = bucket_of(time);
            assert!(bucket < BUCKETS, "time {time} gave bucket {bucket}");
            assert!(
                upper_bound_of(bucket) >= time,
                "time {time} above its bucket"
            );
            if bucket > 0 && bucket < BUCKETS - 1 {
                assert!(
                    upper_bound_of(bucket - 1) < time,
                    "time {time} in a bucket too high"
                );
            }
        }
    }

    #[test]
    fn a_buckets_upper_bound_is_within_a_sixteenth_of_its_times() {
        for time in [16u64, 100, 1_333_333, 999_999_999, 1 << 34] {
            let bound = upper_bound_of(bucket_of(time));
            assert!(bound - time <= time / 16, "time {time}, bound {bound}");
        }
    }

    #[test]
    fn buckets_rise_with_time() {
        let mut last = 0;
        for time in (0..40).map(|power| 3u64 << power) {
            let bucket = bucket_of(time);
            assert!(bucket >= last);
            last = bucket;
        }
    }

    #[test]
    fn an_empty_histogram_has_no_percentile() {
        let snapshot = CallbackTimes::new().snapshot();
        assert_eq!(snapshot.count(), 0);
        assert_eq!(snapshot.maximum(), Duration::ZERO);
        assert_eq!(snapshot.quantile(999, 1000), None);
        assert_eq!(snapshot.share_over(Duration::ZERO), 0.0);
    }

    #[test]
    fn percentiles_and_maximum_read_back_what_was_recorded() {
        let times = CallbackTimes::new();
        // 999 callbacks of 100 µs and one of 900 µs.
        for _ in 0..999 {
            times.record(Duration::from_micros(100));
        }
        times.record(Duration::from_micros(900));
        let snapshot = times.snapshot();
        assert_eq!(snapshot.count(), 1_000);
        assert_eq!(snapshot.maximum(), Duration::from_micros(900));
        // A missing quantile reads as zero, which the assertions below refuse.
        let median = snapshot.quantile(1, 2).unwrap_or_default();
        assert!(median >= Duration::from_micros(100));
        assert!(median <= Duration::from_micros(100) + Duration::from_micros(100) / 16);
        let p999 = snapshot.quantile(999, 1000).unwrap_or_default();
        assert!(p999 >= Duration::from_micros(100) && p999 < Duration::from_micros(107));
        assert_eq!(snapshot.quantile(1, 1), Some(Duration::from_micros(900)));
        assert_eq!(snapshot.share_over(Duration::from_micros(500)), 0.001);
        assert_eq!(snapshot.quantile(3, 2), None);
        // The 0th quantile is the shortest callback's bucket.
        assert_eq!(snapshot.quantile(0, 1), Some(median));
    }

    #[test]
    fn a_time_beyond_the_last_power_keeps_its_exact_maximum() {
        let times = CallbackTimes::new();
        times.record(Duration::from_secs(3_600));
        let snapshot = times.snapshot();
        assert_eq!(snapshot.maximum(), Duration::from_secs(3_600));
        assert_eq!(snapshot.quantile(1, 1), Some(Duration::from_secs(3_600)));
    }
}
