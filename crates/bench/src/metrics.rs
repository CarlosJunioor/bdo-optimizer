//! Pure FPS-metric math computed from a raw frame-time series (milliseconds).
//!
//! This module is the correctness core of the crate and is deliberately free of any
//! IO or platform code so it can be exhaustively unit-tested. Every value is derived
//! from the raw per-frame durations — the source of truth — and never from a stream
//! of pre-averaged instantaneous FPS numbers.
//!
//! # Definitions
//!
//! Given a series of frame times `t[i]` in milliseconds:
//!
//! * **Average FPS** = `N / (Σt / 1000)` — total frames divided by total wall-clock
//!   seconds. This is *not* the arithmetic mean of `1000/t[i]`, which over-weights
//!   short frames and inflates the number.
//! * **Max / Min FPS** = `1000 / min(t)` and `1000 / max(t)` respectively.
//! * **P1 low (percentile method)** = `1000 / percentile(t, 99)`. The 1% *slowest*
//!   frames are the 99th percentile of frame time; inverting gives an FPS floor.
//!   **P0.1 low** = `1000 / percentile(t, 99.9)`.
//! * **1% low (integral / CapFrameX method)** — sort frame times descending, walk
//!   from the slowest frame accumulating durations until the running sum reaches 1%
//!   of the total session duration; the frame time at that cutoff, inverted, is the
//!   metric. This is time-weighted (a stutter's *duration* matters, not just its
//!   rank) and is the most sensitive to the kind of hitching that CPU-affinity
//!   changes affect. **0.1% low integral** uses a 0.1% threshold.
//!
//! Sessions with fewer than [`LOW_CONFIDENCE_FRAMES`] frames set
//! [`Metrics::low_confidence`], because tail percentiles over a tiny sample are noisy.

use serde::{Deserialize, Serialize};

use crate::error::BenchError;

/// Below this frame count the tail (P1 / P0.1 / integral) metrics are statistically
/// shaky, so [`Metrics::low_confidence`] is set for the UI to warn on.
pub const LOW_CONFIDENCE_FRAMES: usize = 1000;

/// A full set of FPS metrics derived from one frame-time series.
///
/// All FPS fields are in frames-per-second; [`Metrics::duration_seconds`] is the total
/// captured wall-clock time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Number of frames in the series.
    pub frame_count: usize,
    /// Total captured duration in seconds (`Σt / 1000`).
    pub duration_seconds: f64,
    /// `N / (Σt / 1000)` — average FPS over the whole session.
    pub avg_fps: f64,
    /// `1000 / min(t)` — fastest single frame.
    pub max_fps: f64,
    /// `1000 / max(t)` — slowest single frame.
    pub min_fps: f64,
    /// `1000 / percentile(t, 99)` — 1% low, percentile method.
    pub p1_low_fps: f64,
    /// `1000 / percentile(t, 99.9)` — 0.1% low, percentile method.
    pub p0_1_low_fps: f64,
    /// CapFrameX-style time-weighted 1% low.
    pub one_percent_low_integral_fps: f64,
    /// CapFrameX-style time-weighted 0.1% low.
    pub zero_1_percent_low_integral_fps: f64,
    /// `true` when `frame_count < LOW_CONFIDENCE_FRAMES`; tail metrics are noisy.
    pub low_confidence: bool,
}

impl Metrics {
    /// Compute all metrics from a frame-time series in milliseconds.
    ///
    /// # Errors
    /// Returns [`BenchError::EmptyInput`] if `frame_times_ms` is empty. Non-positive
    /// or non-finite samples are ignored; if that leaves nothing usable the same
    /// error is returned.
    pub fn from_frame_times(frame_times_ms: &[f64]) -> Result<Metrics, BenchError> {
        // Filter to strictly-positive, finite samples. A zero or negative frame time
        // is physically meaningless and would blow up the reciprocals.
        let mut times: Vec<f64> = frame_times_ms
            .iter()
            .copied()
            .filter(|t| t.is_finite() && *t > 0.0)
            .collect();
        if times.is_empty() {
            return Err(BenchError::EmptyInput);
        }

        times.sort_by(|a, b| a.partial_cmp(b).expect("filtered NaNs above"));

        let frame_count = times.len();
        let sum_ms: f64 = times.iter().sum();
        let duration_seconds = sum_ms / 1000.0;

        // Average FPS = frames / seconds. Never a mean of 1000/t.
        let avg_fps = frame_count as f64 / duration_seconds;

        // Sorted ascending: first is the shortest (fastest) frame, last the longest.
        let min_t = times[0];
        let max_t = times[frame_count - 1];
        let max_fps = 1000.0 / min_t;
        let min_fps = 1000.0 / max_t;

        let p99 = percentile_sorted(&times, 99.0);
        let p999 = percentile_sorted(&times, 99.9);
        let p1_low_fps = 1000.0 / p99;
        let p0_1_low_fps = 1000.0 / p999;

        let one_percent_low_integral_fps = 1000.0 / integral_low_frame_time(&times, sum_ms, 0.01);
        let zero_1_percent_low_integral_fps =
            1000.0 / integral_low_frame_time(&times, sum_ms, 0.001);

        Ok(Metrics {
            frame_count,
            duration_seconds,
            avg_fps,
            max_fps,
            min_fps,
            p1_low_fps,
            p0_1_low_fps,
            one_percent_low_integral_fps,
            zero_1_percent_low_integral_fps,
            low_confidence: frame_count < LOW_CONFIDENCE_FRAMES,
        })
    }
}

/// Smoothed *instantaneous* FPS from the most recent frames.
///
/// Unlike [`Metrics::avg_fps`] (a whole-session, time-weighted average), this is a
/// short-window "what is the frame rate right now" reading for a live overlay:
/// `1000 / mean(last `window` frame times)`. Non-finite / non-positive samples in
/// the window are ignored. Returns `0.0` when there is nothing usable to average.
///
/// A mean of frame times (then inverted) — rather than a mean of `1000/t` — keeps a
/// single long frame from being under-weighted, matching how the session average is
/// defined.
pub fn current_fps(frame_times_ms: &[f64], window: usize) -> f64 {
    if frame_times_ms.is_empty() || window == 0 {
        return 0.0;
    }
    let n = window.min(frame_times_ms.len());
    let recent = &frame_times_ms[frame_times_ms.len() - n..];
    let mut sum = 0.0;
    let mut count = 0usize;
    for &t in recent {
        if t.is_finite() && t > 0.0 {
            sum += t;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    1000.0 / (sum / count as f64)
}

/// Linear-interpolated percentile over an **ascending-sorted** slice.
///
/// Uses the same "linear interpolation between closest ranks" definition as NumPy's
/// default (`rank = p/100 * (n - 1)`), which is the convention CapFrameX and most
/// benchmark tooling report. `pct` is clamped to `[0, 100]`.
///
/// # Panics
/// Never for a non-empty slice; callers guarantee `sorted` is non-empty.
fn percentile_sorted(sorted: &[f64], pct: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let p = pct.clamp(0.0, 100.0);
    let rank = p / 100.0 * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// CapFrameX-style time-weighted "low" frame time.
///
/// Walks frame times from slowest to fastest, accumulating their durations until the
/// running total reaches `threshold` of the whole session (`0.01` = 1%). Returns the
/// frame time at that cutoff — invert it (`1000 / result`) to get FPS.
///
/// `sorted` must be ascending; we iterate it in reverse (descending) so the slowest
/// frames — the stutters — are counted first, which is what makes this metric
/// duration-weighted rather than count-weighted.
fn integral_low_frame_time(sorted: &[f64], total_ms: f64, threshold: f64) -> f64 {
    let budget = total_ms * threshold;
    let mut accum = 0.0;
    // Reverse iteration = descending order.
    for &t in sorted.iter().rev() {
        accum += t;
        if accum >= budget {
            return t;
        }
    }
    // Threshold never reached (e.g. a single frame > budget is caught on iter 1, so
    // this only happens for degenerate all-tiny series): fall back to the slowest.
    *sorted.last().expect("non-empty by contract")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn empty_input_errors() {
        assert!(matches!(
            Metrics::from_frame_times(&[]),
            Err(BenchError::EmptyInput)
        ));
    }

    #[test]
    fn all_nonpositive_errors() {
        assert!(matches!(
            Metrics::from_frame_times(&[0.0, -5.0, f64::NAN]),
            Err(BenchError::EmptyInput)
        ));
    }

    #[test]
    fn uniform_16667_is_60fps() {
        // 2000 frames of 1000/60 ms → every metric ≈ 60 FPS.
        let t = 1000.0 / 60.0;
        let frames = vec![t; 2000];
        let m = Metrics::from_frame_times(&frames).unwrap();
        assert_eq!(m.frame_count, 2000);
        assert!(approx(m.avg_fps, 60.0, 1e-6));
        assert!(approx(m.max_fps, 60.0, 1e-6));
        assert!(approx(m.min_fps, 60.0, 1e-6));
        assert!(approx(m.p1_low_fps, 60.0, 1e-6));
        assert!(approx(m.p0_1_low_fps, 60.0, 1e-6));
        assert!(approx(m.one_percent_low_integral_fps, 60.0, 1e-6));
        assert!(approx(m.zero_1_percent_low_integral_fps, 60.0, 1e-6));
        assert!(!m.low_confidence);
        assert!(approx(m.duration_seconds, 2000.0 * t / 1000.0, 1e-9));
    }

    #[test]
    fn low_confidence_flag_under_threshold() {
        let frames = vec![16.0; 500];
        let m = Metrics::from_frame_times(&frames).unwrap();
        assert!(m.low_confidence);
    }

    #[test]
    fn avg_is_time_weighted_not_mean_of_instantaneous() {
        // Two frames: 10ms (100fps) and 30ms (33.3fps).
        // Time-weighted avg = 2 frames / 0.040 s = 50 fps.
        // Naive mean of instantaneous = (100 + 33.33)/2 = 66.7 fps — which we must NOT get.
        let m = Metrics::from_frame_times(&[10.0, 30.0]).unwrap();
        assert!(approx(m.avg_fps, 50.0, 1e-9));
        assert!((m.avg_fps - 66.666).abs() > 10.0);
    }

    #[test]
    fn min_max_fps_from_spike() {
        // A steady 10ms series with one 100ms spike.
        let mut frames = vec![10.0; 100];
        frames[50] = 100.0;
        let m = Metrics::from_frame_times(&frames).unwrap();
        assert!(approx(m.max_fps, 100.0, EPS)); // 1000/10
        assert!(approx(m.min_fps, 10.0, EPS)); // 1000/100
    }

    #[test]
    fn percentile_known_values() {
        // 1..=100 ascending. NumPy-linear p99 over 0-based ranks:
        // rank = 0.99 * 99 = 98.01 → between index 98 (=99) and 99 (=100) → 99.01.
        let v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert!(approx(percentile_sorted(&v, 99.0), 99.01, 1e-9));
        // p0 = min, p100 = max, p50 midpoint = 50.5.
        assert!(approx(percentile_sorted(&v, 0.0), 1.0, 1e-9));
        assert!(approx(percentile_sorted(&v, 100.0), 100.0, 1e-9));
        assert!(approx(percentile_sorted(&v, 50.0), 50.5, 1e-9));
    }

    #[test]
    fn percentile_single_element() {
        assert!(approx(percentile_sorted(&[42.0], 99.0), 42.0, 1e-12));
    }

    #[test]
    fn p1_low_matches_percentile_definition() {
        // 100 frames at 10ms + implicit: use 99 frames 10ms and 1 frame 50ms.
        let mut frames = vec![10.0; 99];
        frames.push(50.0);
        let m = Metrics::from_frame_times(&frames).unwrap();
        // sorted: [10 x99, 50]. p99 rank = 0.99*99 = 98.01 → between idx98(10) idx99(50)
        // = 10 + 40*0.01 = 10.4 → p1_low = 1000/10.4.
        assert!(approx(m.p1_low_fps, 1000.0 / 10.4, 1e-6));
    }

    #[test]
    fn integral_low_captures_stutter() {
        // 1000 frames: 999 at 10ms (total 9990ms) + 1 at 500ms. Total = 10490ms.
        // 1% budget = 104.9ms. Descending: first frame is 500ms ≥ 104.9 → cutoff=500ms.
        // one_percent_low_integral = 1000/500 = 2 fps.
        let mut frames = vec![10.0; 999];
        frames.push(500.0);
        let m = Metrics::from_frame_times(&frames).unwrap();
        assert!(approx(m.one_percent_low_integral_fps, 2.0, 1e-9));
    }

    #[test]
    fn integral_low_multi_frame_accumulation() {
        // total = 100 frames * 10ms = 1000ms. 1% budget = 10ms.
        // Descending all 10ms: first frame accum=10 ≥ 10 → cutoff 10ms → 100 fps.
        let frames = vec![10.0; 100];
        let ft = integral_low_frame_time(
            &{
                let mut v = frames.clone();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v
            },
            1000.0,
            0.01,
        );
        assert!(approx(ft, 10.0, 1e-12));
    }

    #[test]
    fn duration_and_count_correct() {
        let frames = vec![16.0, 16.0, 16.0, 16.0]; // 64ms total
        let m = Metrics::from_frame_times(&frames).unwrap();
        assert_eq!(m.frame_count, 4);
        assert!(approx(m.duration_seconds, 0.064, 1e-12));
    }

    #[test]
    fn ignores_invalid_samples_but_keeps_valid() {
        let frames = vec![16.0, f64::NAN, -3.0, 0.0, 16.0];
        let m = Metrics::from_frame_times(&frames).unwrap();
        assert_eq!(m.frame_count, 2);
    }

    #[test]
    fn current_fps_empty_is_zero() {
        assert_eq!(current_fps(&[], 30), 0.0);
        assert_eq!(current_fps(&[16.0], 0), 0.0);
    }

    #[test]
    fn current_fps_uses_only_recent_window() {
        // 100 old frames at 100ms (10 fps) then 30 recent at 1000/60 ms (60 fps).
        let mut frames = vec![100.0; 100];
        frames.extend(vec![1000.0 / 60.0; 30]);
        // Window of 30 sees only the fast tail → ~60 fps, not dragged down by the tenths.
        assert!(approx(current_fps(&frames, 30), 60.0, 1e-6));
    }

    #[test]
    fn current_fps_ignores_invalid_in_window() {
        let frames = vec![f64::NAN, -1.0, 10.0];
        // Only the 10ms frame counts → 100 fps.
        assert!(approx(current_fps(&frames, 3), 100.0, 1e-9));
    }
}
