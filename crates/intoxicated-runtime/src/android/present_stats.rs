//! Frame pacing: the time between `vkQueuePresentKHR` calls.
//!
//! Recorded on the render thread, once per engine present and before it is
//! forwarded, with nothing that can block or allocate: a clock read, a few
//! relaxed atomics and one store into a ring of the last 1024 intervals.
//! Read by the pump's 30 s health line and by devctl `info`, which sort a copy.
//!
//! The ring holds about 4 s at 240 fps, so a summary says nothing about a
//! stall older than that: a 2.9 s frame 47 s into a run showed in a devctl
//! sample at 50 s and was gone from the health line at 60 s (measured). The
//! worst interval since the previous health line is kept beside the ring for
//! that reason, and the health line reports it as `worst30s=`.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::Instant;

/// Intervals kept.
const RING: usize = 1024;
/// Fewer intervals than this is no summary.
const MIN_INTERVALS: usize = 32;

/// One recorder. The process has one ([`STATS`]); tests make their own.
pub(crate) struct PresentStats {
    /// The last present, in nanoseconds since the clock's base, plus one so
    /// that 0 always means "none yet".
    last_ns: AtomicU64,
    /// Intervals recorded since the start; slot = count mod [`RING`].
    recorded: AtomicU64,
    /// Interval lengths in microseconds, saturated at `u32::MAX` (71.6 min).
    ring: [AtomicU32; RING],
    /// The longest interval since [`Self::take_worst`] last ran, in
    /// microseconds; 0 for none.
    worst_us: AtomicU32,
}

impl PresentStats {
    pub(crate) const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: AtomicU32 = AtomicU32::new(0);
        PresentStats {
            last_ns: AtomicU64::new(0),
            recorded: AtomicU64::new(0),
            ring: [ZERO; RING],
            worst_us: AtomicU32::new(0),
        }
    }

    /// One present at `now_ns` on a monotonic clock. The first present, and
    /// one that is not later than the last, record nothing.
    pub(crate) fn record_at(&self, now_ns: u64) {
        let now = now_ns.saturating_add(1);
        let previous = self.last_ns.swap(now, Relaxed);
        if previous == 0 || now <= previous {
            return;
        }
        let us = ((now - previous) / 1000).min(u32::MAX as u64) as u32;
        let i = self.recorded.fetch_add(1, Relaxed);
        self.ring[(i % RING as u64) as usize].store(us, Relaxed);
        self.worst_us.fetch_max(us, Relaxed);
    }

    /// `frames n=... p50=... p95=... p99=... max=... median_fps=...` over the
    /// intervals in the ring, or `None` before there are 32.
    pub(crate) fn summary(&self) -> Option<String> {
        let recorded = self.recorded.load(Relaxed) as usize;
        if recorded < MIN_INTERVALS {
            return None;
        }
        let mut v: Vec<u32> = self.ring[..recorded.min(RING)]
            .iter()
            .map(|s| s.load(Relaxed))
            .filter(|&us| us != 0)
            .collect();
        if v.len() < MIN_INTERVALS {
            return None;
        }
        v.sort_unstable();
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize] as f64 / 1000.0;
        let p50 = at(0.50);
        let fps = if p50 > 0.0 { 1000.0 / p50 } else { 0.0 };
        Some(format!(
            "frames n={} p50={:.1}ms p95={:.1}ms p99={:.1}ms max={:.1}ms median_fps={:.0}",
            v.len(),
            p50,
            at(0.95),
            at(0.99),
            at(1.0),
            fps
        ))
    }

    /// The longest interval since the last call, in milliseconds, and start
    /// the next period. `None` when no interval ended in between.
    pub(crate) fn take_worst(&self) -> Option<f64> {
        let us = self.worst_us.swap(0, Relaxed);
        (us != 0).then_some(us as f64 / 1000.0)
    }

    /// The same without starting a new period, for devctl.
    pub(crate) fn peek_worst(&self) -> Option<f64> {
        let us = self.worst_us.load(Relaxed);
        (us != 0).then_some(us as f64 / 1000.0)
    }

    /// What the health line appends: the summary, then the worst interval
    /// of the period it closes. Starts the next period.
    pub(crate) fn health(&self) -> Option<String> {
        let worst = self.take_worst().map(|ms| format!("worst30s={ms:.1}ms"));
        match (self.summary(), worst) {
            (Some(s), Some(w)) => Some(format!("{s} {w}")),
            (s, w) => s.or(w),
        }
    }
}

static STATS: PresentStats = PresentStats::new();

fn now_ns() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// One engine present. Called by the present hook before it forwards.
pub fn record() {
    STATS.record_at(now_ns());
}

/// The summary over the last 1024 intervals; see [`PresentStats::summary`].
pub fn summary() -> Option<String> {
    STATS.summary()
}

/// The health line's summary plus `worst30s=`, closing the period.
pub fn health_summary() -> Option<String> {
    STATS.health()
}

/// The worst interval of the period the next health line will close, in
/// milliseconds; 0.0 when none has ended yet.
pub fn worst_since_health() -> f64 {
    STATS.peek_worst().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn presents_at(times_ms: impl IntoIterator<Item = u64>) -> PresentStats {
        let s = PresentStats::new();
        for t in times_ms {
            s.record_at(t * MS);
        }
        s
    }

    #[test]
    fn thirty_two_intervals_make_a_summary_and_thirty_one_do_not() {
        assert!(presents_at(0..32).summary().is_none(), "32 presents are 31 intervals");
        let s = presents_at(0..33).summary().expect("33 presents are 32 intervals");
        assert!(s.starts_with("frames n=32 p50=1.0ms"), "{s}");
    }

    #[test]
    fn percentiles_are_the_rounded_index_of_the_sorted_intervals() {
        // Intervals 1, 2, ..., 100 ms: presents at the triangular numbers.
        let mut t = 0;
        let mut times = vec![0];
        for i in 1..=100 {
            t += i;
            times.push(t);
        }
        assert_eq!(
            presents_at(times).summary().unwrap(),
            "frames n=100 p50=51.0ms p95=95.0ms p99=99.0ms max=100.0ms median_fps=20"
        );
    }

    #[test]
    fn only_the_last_1024_intervals_count() {
        // 976 slow intervals, then 1024 fast ones: the slow ones are gone.
        let s = PresentStats::new();
        let mut t = 0;
        s.record_at(0);
        for i in 0..2000 {
            t += if i < 976 { 50 * MS } else { 4 * MS };
            s.record_at(t);
        }
        let line = s.summary().unwrap();
        assert!(line.starts_with("frames n=1024 p50=4.0ms"), "{line}");
        assert!(line.contains("max=4.0ms"), "{line}");
    }

    #[test]
    fn an_interval_too_long_for_the_slot_saturates() {
        let s = PresentStats::new();
        s.record_at(0);
        s.record_at(72 * 60 * 1000 * MS);
        assert_eq!(s.ring[0].load(Relaxed), u32::MAX);
        assert_eq!(s.take_worst(), Some(u32::MAX as f64 / 1000.0));
    }

    #[test]
    fn time_that_does_not_advance_records_nothing() {
        let s = PresentStats::new();
        s.record_at(5 * MS);
        s.record_at(5 * MS);
        s.record_at(4 * MS);
        assert_eq!(s.recorded.load(Relaxed), 0);
        s.record_at(9 * MS);
        assert_eq!(s.recorded.load(Relaxed), 1);
        assert_eq!(s.ring[0].load(Relaxed), 5000);
    }

    #[test]
    fn the_first_present_at_time_zero_still_starts_the_clock() {
        let s = presents_at([0, 4]);
        assert_eq!(s.recorded.load(Relaxed), 1);
        assert_eq!(s.ring[0].load(Relaxed), 4000);
    }

    #[test]
    fn the_worst_frame_of_a_period_survives_the_ring_and_resets_per_line() {
        // A 2.9 s stall, then 2000 frames at about 240 fps: long gone from
        // the ring, still the period's worst.
        let s = PresentStats::new();
        let mut t = 0;
        s.record_at(t);
        t += 2900 * MS;
        s.record_at(t);
        for _ in 0..2000 {
            t += 4_200_000;
            s.record_at(t);
        }
        let line = s.health().unwrap();
        assert!(line.contains("max=4.2ms"), "{line}");
        assert!(line.ends_with(" worst30s=2900.0ms"), "{line}");
        // The next period starts empty, and devctl's peek does not end it.
        assert_eq!(s.peek_worst(), None);
        t += 6 * MS;
        s.record_at(t);
        assert_eq!(s.peek_worst(), Some(6.0));
        assert_eq!(s.peek_worst(), Some(6.0));
        assert!(s.health().unwrap().ends_with(" worst30s=6.0ms"));
        // A period in which nothing was presented adds nothing.
        let quiet = s.health().unwrap();
        assert!(!quiet.contains("worst30s"), "{quiet}");
    }

    #[test]
    fn a_period_too_short_for_a_summary_still_reports_its_worst() {
        let s = presents_at([0, 3, 10]);
        assert_eq!(s.health().as_deref(), Some("worst30s=7.0ms"));
        assert_eq!(PresentStats::new().health(), None);
    }
}
