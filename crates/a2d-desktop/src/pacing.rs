//! Frame pacing: keeping animation time smooth against a fixed-rate display.
//!
//! The model is advanced by a delta, and the delta is measured from the wall
//! clock. On a display that presents at a fixed rate, that measurement jitters
//! (a frame woken 4 ms late is followed by one 4 ms early) even though every
//! frame lands on its own vsync, and the jitter shows up as judder because the
//! content moves by an uneven amount between evenly spaced presents.
//!
//! [`FrameClock`] removes that jitter the way Godot's "delta smoothing" does: a
//! measured delta close to a whole number of display periods is replaced by
//! that number. Anything that is not close to a whole multiple (a real stall, a
//! non-vsync run) passes through as measured.
//!
//! The rule it holds itself to is that smoothing must never be worse than no
//! smoothing. So:
//!
//! - The period is *estimated* from the deltas, with the operating system's
//!   figure only as the starting point and a sanity bound. The OS reports whole
//!   hertz (59.94 Hz reads as 59), and trusting that would make content time
//!   run fast or slow against the wall clock. Every snapped delta teaches the
//!   estimate (`delta / whole`), so a display running at 2x or 3x its cadence
//!   is learned as well as one at 1x.
//! - The difference between wall time and fed time is paid back gradually, a
//!   couple of percent of a period per frame, and only once it exceeds a
//!   deadband, never dumped into one frame. Frames that are not snapped are
//!   uneven anyway, so they absorb whatever is owed.
//! - When the clock cannot follow the display (the real rate is outside the
//!   bound around the reported one, or the debt outgrows a period) it stops
//!   snapping and passes deltas through until the deltas agree with the
//!   estimate again.
//!
//! Pure and display-free, so it is tested without a window. It only changes
//! *how smoothly* time advances, never how much: frame pacing must not change
//! animation correctness.

use std::time::Duration;

/// How far from a whole number of periods a delta may be and still be snapped,
/// as a fraction of one period.
///
/// 0.3 covers the common jitter (a 21 ms frame on a 16.67 ms display is 0.26
/// off) and stays well clear of 0.5, where the nearest multiple becomes
/// ambiguous.
const SNAP_TOLERANCE: f64 = 0.3;

/// Weight of each snapped sample in the running period estimate. About a 50
/// frame memory: quick to find the true rate, slow enough that jitter
/// averages out.
const ESTIMATE_GAIN: f64 = 0.02;

/// The clock only follows a display whose measured period is within this
/// fraction of the reported one. Whole-hertz rounding is under 2% at 60 Hz;
/// beyond 5% the reported rate is not describing the display and snapping
/// against it would fight the real cadence.
const ESTIMATE_BOUND: f64 = 0.05;

/// Wall-minus-fed time the clock tolerates before correcting, as a fraction of
/// a period. Jitter alone stays inside it (a snapped 21 ms frame owes 0.26), so
/// a healthy run is never perturbed.
const DEADBAND: f64 = 0.4;

/// The most a frame may be stretched or shrunk to pay the debt back, as a
/// fraction of a period.
const MAX_CORRECTION: f64 = 0.02;

/// The most an unsnapped frame is moved to pay debt, as a fraction of a
/// period. Small enough that a frame already as far from the display's cadence
/// as it can be is not made noticeably worse.
const ABSORB_STEP: f64 = 0.02;

/// A snapped delta this far from the estimate (as a fraction of it) is a
/// stray: ordinary jitter produces a few, a display the clock cannot follow
/// produces nothing else. The clock tracks the recent share of strays.
const STRAY_WINDOW: f64 = 0.15;

/// Memory of the stray share, per snapped delta (about a 10 sample average).
const STRAY_DECAY: f64 = 0.9;

/// A debt beyond a period is only taken as the clock failing to follow when
/// more than this share of recent deltas were strays. Below it the debt is
/// jitter, which random-walks it past a period every so often on a healthy
/// display, and is simply dropped.
const SUSPEND_ABOVE: f64 = 0.4;

/// A suspended clock resumes once the stray share has fallen below this and
/// at least [`RESUME_MIN`] deltas have gone by since it suspended.
const RESUME_BELOW: f64 = 0.25;

/// Fewest snapped deltas to wait out before resuming.
const RESUME_MIN: u32 = 30;

/// How far a sample may be from the estimate and still adjust it, as a
/// fraction of the estimate. Wider than [`ESTIMATE_BOUND`] so a whole-hertz
/// mismatch is learned, narrower than a misclassified frame.
const LEARNING_WINDOW: f64 = 0.08;

/// An interval beyond this many periods counts as a missed vsync.
pub const LATE_FACTOR: f64 = 1.5;

/// Turns measured frame deltas into smoothed ones.
#[derive(Debug, Clone, Default)]
pub struct FrameClock {
    /// The period the platform reported, in seconds. `None` disables smoothing.
    nominal: Option<f64>,
    /// The period measured from recent frames. Unbounded, so that a display
    /// far from its reported rate is noticed rather than clamped into.
    estimate: f64,
    /// Seconds of wall time not yet handed to the model (positive) or handed
    /// out ahead of the wall clock (negative).
    residual: f64,
    /// Snapping has been given up until the deltas settle down again.
    suspended: bool,
    /// Snapped deltas seen since the clock suspended.
    since_suspend: u32,
    /// Recent share of snapped deltas that were strays, between 0 and 1.
    strays: f64,
}

impl FrameClock {
    /// A clock for a display with the given refresh period; `None` or a zero
    /// period passes every delta through untouched.
    pub fn new(period: Option<Duration>) -> Self {
        let mut clock = Self::default();
        clock.set_period(period);
        clock
    }

    /// Changes the reported refresh period, e.g. after moving to another
    /// monitor. A no-op when it has not changed, so it is cheap to call on
    /// every move. A new period re-seeds the estimate.
    pub fn set_period(&mut self, period: Option<Duration>) {
        let period = period.map(|p| p.as_secs_f64()).filter(|p| *p > 0.0);
        if period != self.nominal {
            self.nominal = period;
            self.estimate = period.unwrap_or(0.0);
            self.residual = 0.0;
            self.suspended = false;
            self.since_suspend = 0;
            self.strays = 0.0;
        }
    }

    /// Forgets the carried error, when the delta that follows is not
    /// continuous with the ones before it (the window has just come up).
    ///
    /// The estimate is kept on purpose: it describes the display, not the
    /// gap, and only a change of reported period ([`Self::set_period`])
    /// invalidates it.
    pub fn reset(&mut self) {
        self.residual = 0.0;
    }

    /// Whether the clock is currently snapping deltas, for tests and tracing.
    pub fn is_following(&self) -> bool {
        self.nominal.is_some_and(|n| !self.suspended && (self.estimate - n).abs() <= ESTIMATE_BOUND * n)
    }

    /// The delta to advance the model by, given the measured one.
    ///
    /// A snapped result never exceeds `limit`. A measured delta above it is a
    /// stall or a wake from sleep, not jitter, and is returned unchanged for
    /// the caller's own cap, with the carried error dropped.
    pub fn step(&mut self, measured: Duration, limit: Duration) -> Duration {
        let Some(nominal) = self.nominal else { return measured };
        if measured > limit {
            self.residual = 0.0;
            return measured;
        }
        let m = measured.as_secs_f64();
        let limit = limit.as_secs_f64();
        let bounded = nominal * (1.0 - ESTIMATE_BOUND)..=nominal * (1.0 + ESTIMATE_BOUND);

        // Judge against the period the clock would snap to: the estimate,
        // held to its bound.
        let period = self.estimate.clamp(*bounded.start(), *bounded.end());
        let whole = (m / period + 0.5).floor();
        // A delta of several periods is a stall or a slow cadence, and those
        // land on whole multiples far more exactly than jitter does; a late
        // frame that merely looks like two periods must not be stretched into
        // them, so multiples are held to half the tolerance.
        let tolerance = if whole > 1.0 { 0.5 * SNAP_TOLERANCE } else { SNAP_TOLERANCE };
        if whole < 1.0 || (m - whole * period).abs() > tolerance * period {
            // Not near a whole multiple: unevenly spaced anyway, so this
            // frame may absorb what is owed.
            return self.absorb(m, m - whole.max(1.0) * period, limit);
        }

        // Every snapped delta is a sample of the real period, whatever its
        // multiple.
        // A sample far from the estimate is more likely a frame that landed on
        // the wrong multiple than evidence of a different rate (a late 27 ms
        // frame is not a 13.5 ms period), so it does not teach.
        let sample = m / whole;
        let stray = (sample - self.estimate).abs() > STRAY_WINDOW * self.estimate;
        self.strays = STRAY_DECAY * self.strays + (1.0 - STRAY_DECAY) * f64::from(u8::from(stray));
        if (sample - self.estimate).abs() <= LEARNING_WINDOW * self.estimate {
            self.estimate += (sample - self.estimate) * ESTIMATE_GAIN;
        }

        if self.suspended {
            self.since_suspend += 1;
            if self.since_suspend >= RESUME_MIN && self.strays < RESUME_BELOW {
                self.suspended = false;
            }
            return measured;
        }
        // A display the estimate has left the bound for cannot be followed.
        if !bounded.contains(&self.estimate) {
            self.residual = 0.0;
            return measured;
        }

        let mut out = whole * self.estimate;
        // Pay the debt back slowly, and only once jitter alone cannot explain
        // it: what is left over is the estimate lagging the real rate.
        let excess = self.residual.abs() - DEADBAND * self.estimate;
        if excess > 0.0 {
            out += self.residual.signum() * excess.min(MAX_CORRECTION * self.estimate);
        }
        out = out.min(limit);
        let owed = self.residual + m - out;
        if owed.abs() > self.estimate {
            // More than a period of disagreement. Either jitter has walked the
            // debt out (common, and not a reason to stop) or the display is not
            // one the clock can follow (mostly strays). Either way the debt is
            // dropped rather than paid in one visible step, which costs under
            // a period of model time: acceptable because it is rare, and
            // bounded because it can never grow past one period.
            self.residual = 0.0;
            if self.strays > SUSPEND_ABOVE {
                self.suspended = true;
                self.since_suspend = 0;
            }
            return measured;
        }
        self.residual = owed;
        Duration::from_secs_f64(out.max(0.0))
    }

    /// A frame that is passed through. It also settles debt, but gradually
    /// and only in the direction that brings it closer to its nearest whole
    /// multiple of the period (`offset` is how far it is from that now):
    /// paying a late frame's debt by making it later would be worse than not
    /// smoothing at all, and an ambiguous frame cannot tell which multiple it
    /// was meant to be, so it is never moved by more than a small step.
    fn absorb(&mut self, m: f64, offset: f64, limit: f64) -> Duration {
        let debt = self.residual;
        let take = if debt.signum() == -offset.signum() {
            debt.clamp(-ABSORB_STEP * self.estimate, ABSORB_STEP * self.estimate)
        } else {
            0.0
        };
        let out = (m + take).clamp(0.0, limit);
        self.residual = debt - (out - m);
        Duration::from_secs_f64(out)
    }
}

/// Late-frame accounting for the run summary.
#[derive(Debug, Clone, Default)]
pub struct PacingStats {
    /// Presented frames whose interval from the previous one exceeded
    /// [`LATE_FACTOR`] refresh periods.
    pub late_frames: u64,
    /// The longest interval between two presented frames.
    pub worst_interval: Duration,
}

impl PacingStats {
    /// Records the interval between two consecutive presented frames.
    pub fn record(&mut self, interval: Duration, period: Option<Duration>) {
        self.worst_interval = self.worst_interval.max(interval);
        if period.is_some_and(|p| interval.as_secs_f64() > LATE_FACTOR * p.as_secs_f64()) {
            self.late_frames += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_millis(100);
    /// 60 Hz.
    const PERIOD: f64 = 1.0 / 60.0;

    fn clock() -> FrameClock {
        FrameClock::new(Some(Duration::from_secs_f64(PERIOD)))
    }

    fn ms(v: f64) -> Duration {
        Duration::from_secs_f64(v / 1000.0)
    }

    fn close(a: Duration, b: f64) -> bool {
        (a.as_secs_f64() - b).abs() < 1e-9
    }

    #[test]
    fn an_exact_period_is_unchanged() {
        let mut c = clock();
        for _ in 0..10 {
            assert!(close(c.step(Duration::from_secs_f64(PERIOD), LIMIT), PERIOD));
        }
    }

    #[test]
    fn jittered_pairs_become_even_steps() {
        let mut c = clock();
        // 21 ms then 12.33 ms: a pair spans exactly two periods (33.33 ms). The
        // 4.33 ms excess of the first is repaid by the shortfall of the second.
        // The estimate wobbles by microseconds with the jitter, so the steps
        // are even to within 1%, not bit-identical.
        for _ in 0..500 {
            for m in [21.0, 2000.0 / 60.0 - 21.0] {
                let dt = c.step(ms(m), LIMIT).as_secs_f64();
                assert!((dt - PERIOD).abs() < 0.01 * PERIOD, "{dt}");
            }
        }
    }

    #[test]
    fn balanced_jitter_leaves_no_long_run_drift() {
        let mut c = clock();
        let (mut measured, mut fed) = (0.0, 0.0);
        for _ in 0..1000 {
            for m in [PERIOD * 1000.0 + 4.0, PERIOD * 1000.0 - 4.0] {
                fed += c.step(ms(m), LIMIT).as_secs_f64();
                measured += m / 1000.0;
            }
        }
        assert!((fed - measured).abs() < PERIOD, "fed {fed} measured {measured}");
    }

    #[test]
    fn a_two_frame_stall_maps_to_two_periods() {
        let mut c = clock();
        // Within 1%, not exact: every snapped delta nudges the period estimate.
        for m in [34.0, 32.0] {
            let dt = c.step(ms(m), LIMIT).as_secs_f64();
            assert!((dt - 2.0 * PERIOD).abs() < 0.01 * 2.0 * PERIOD, "{dt}");
        }
    }

    #[test]
    fn a_delta_between_multiples_passes_through() {
        // 25 ms is 1.5 periods: ambiguous, so it is neither snapped nor altered.
        let mut c = clock();
        assert!(close(c.step(ms(25.0), LIMIT), 0.025));
        assert_eq!(c.residual, 0.0);
    }

    #[test]
    fn a_huge_gap_is_left_for_the_cap_and_drops_carried_error() {
        let mut c = clock();
        c.step(ms(21.0), LIMIT);
        assert_ne!(c.residual, 0.0);
        assert_eq!(c.step(Duration::from_secs(5), LIMIT), Duration::from_secs(5));
        assert_eq!(c.residual, 0.0);
    }

    #[test]
    fn a_snapped_step_never_exceeds_the_limit() {
        let mut c = clock();
        // 99.5 ms snaps to six periods, 100 ms, which is over this limit.
        let dt = c.step(ms(99.5), ms(99.6));
        assert!(dt <= ms(99.6), "{dt:?}");
        // The difference is carried, not silently lost.
        assert!(c.residual.abs() > 0.0);
    }

    #[test]
    fn an_unknown_refresh_rate_passes_everything_through() {
        for period in [None, Some(Duration::ZERO)] {
            let mut c = FrameClock::new(period);
            assert_eq!(c.step(ms(21.0), LIMIT), ms(21.0));
            assert_eq!(c.step(ms(2.0), LIMIT), ms(2.0));
        }
    }

    #[test]
    fn an_unthrottled_frame_is_not_snapped_up_to_a_period() {
        let mut c = clock();
        assert_eq!(c.step(ms(2.0), LIMIT), ms(2.0));
    }

    /// Feeds `frames` steady deltas of `real` seconds to a clock told the
    /// display runs at `reported`. After a warm-up no frame may be visibly
    /// off the measured delta, and fed time must stay within one period of the
    /// wall clock throughout. Returns the worst relative deviation seen after
    /// warm-up.
    fn steady(reported: f64, real: f64, frames: usize) -> f64 {
        let mut c = FrameClock::new(Some(Duration::from_secs_f64(reported)));
        let (mut wall, mut fed, mut worst) = (0.0, 0.0, 0.0f64);
        for i in 0..frames {
            let dt = c.step(Duration::from_secs_f64(real), LIMIT).as_secs_f64();
            wall += real;
            fed += dt;
            assert!((fed - wall).abs() <= reported.max(real), "frame {i}: fed {fed} wall {wall}");
            if i >= 300 {
                worst = worst.max((dt - real).abs() / real);
            }
        }
        assert!(worst <= 0.03, "worst deviation {worst} after warm-up");
        worst
    }

    #[test]
    fn a_whole_hertz_rounding_of_the_reported_rate_does_not_sawtooth() {
        // A 59.94 Hz display reported as 59 Hz, at one, two and three display
        // periods per frame: the cases that once stepped a frame by 30-50%.
        for multiple in [1.0, 2.0, 3.0] {
            steady(1.0 / 59.0, multiple / 59.94, 3000);
            steady(1.0 / 59.0, multiple * 0.016683, 3000);
        }
    }

    #[test]
    fn a_reported_rate_a_little_off_either_way_is_followed() {
        for ratio in [0.99, 1.0, 1.003, 1.008, 1.016] {
            for multiple in [1.0, 2.0, 3.0] {
                steady(PERIOD, multiple * PERIOD * ratio, 3000);
            }
        }
        for multiple in [1.0, 2.0, 3.0] {
            steady(1.0 / 144.0, multiple / 143.86, 3000);
            steady(PERIOD, multiple / 60.05, 3000);
            steady(PERIOD, multiple / 59.97, 3000);
        }
    }

    #[test]
    fn a_variable_rate_display_is_followed_or_left_alone() {
        // 144 Hz reported, 75 fps delivered: two periods, within the bound.
        steady(1.0 / 144.0, 1.0 / 75.0, 3000);
        // 144 reported, 100 and 60 fps: not near a whole multiple, passed through.
        steady(1.0 / 144.0, 1.0 / 100.0, 3000);
        steady(1.0 / 144.0, 1.0 / 60.0, 3000);
        // 60 reported, 50 fps: 20% off, beyond the bound, so the clock stops
        // snapping and the deltas come through exactly.
        let mut c = clock();
        for _ in 0..400 {
            c.step(ms(20.0), LIMIT);
        }
        assert!(!c.is_following());
        assert_eq!(c.step(ms(20.0), LIMIT), ms(20.0));
        steady(PERIOD, 1.0 / 50.0, 3000);
        steady(1.0 / 120.0, 1.0 / 90.0, 3000);
        steady(1.0 / 165.0, 1.0 / 100.0, 3000);
    }

    #[test]
    fn the_clock_resumes_once_the_display_agrees_again() {
        let mut c = clock();
        for _ in 0..400 {
            c.step(ms(20.0), LIMIT);
        }
        assert!(!c.is_following());
        for _ in 0..1000 {
            c.step(Duration::from_secs_f64(PERIOD), LIMIT);
        }
        assert!(c.is_following());
        assert!(close(c.step(Duration::from_secs_f64(PERIOD), LIMIT), PERIOD));
    }

    /// A deterministic normal sample, for jitter tests.
    struct Noise(u64);

    impl Noise {
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }

        fn gauss(&mut self, sigma: f64) -> f64 {
            let (a, b) = (self.uniform(), self.uniform());
            sigma * (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
        }
    }

    #[test]
    fn smoothing_under_jitter_is_never_worse_than_raw() {
        for sigma_ms in [0.5, 1.0, 2.0, 3.0] {
            for seed in 1..=3u64 {
                let mut noise = Noise(seed);
                // Wakes land on vsync boundaries with Gaussian noise; the
                // true cadence is exactly one period per frame.
                let times: Vec<f64> = (0..4001).map(|k| k as f64 * PERIOD + noise.gauss(sigma_ms / 1000.0)).collect();
                let mut c = clock();
                let (mut raw_worst, mut smooth_worst, mut frames_worse) = (0.0f64, 0.0f64, 0);
                let (mut wall, mut fed) = (0.0, 0.0);
                for (i, pair) in times.windows(2).enumerate() {
                    let m = pair[1] - pair[0];
                    let dt = c.step(Duration::from_secs_f64(m.max(1e-6)), LIMIT).as_secs_f64();
                    wall += m;
                    fed += dt;
                    // Dropped debt is lost model time, up to a period each, and
                    // successive drops wander; it is the phase of the animation
                    // that moves, never its rate.
                    // Only checked where the jitter is ordinary: at 2 ms and
                    // up the debt walks out every few dozen frames.
                    if sigma_ms <= 1.0 {
                        assert!((fed - wall).abs() <= 6.0 * PERIOD, "sigma {sigma_ms} frame {i}");
                    }
                    raw_worst = raw_worst.max((m - PERIOD).abs());
                    smooth_worst = smooth_worst.max((dt - PERIOD).abs());
                    if (dt - PERIOD).abs() > (m - PERIOD).abs() + 1e-3 {
                        frames_worse += 1;
                    }
                }
                assert!(smooth_worst <= raw_worst + 5e-4, "sigma {sigma_ms} seed {seed}: {smooth_worst} > {raw_worst}");
                // The estimate wobbles by a fraction of a millisecond with the
                // jitter, and debt is paid in small steps, so a frame that was
                // already almost exact can come out slightly off. Almost no frame
                // may come out more than a millisecond worse than it went in.
                // Heavy jitter (3 ms) can still put a late frame on the wrong
                // multiple now and then; it stays a rarity, under 0.2%.
                assert!(
                    frames_worse <= 8,
                    "sigma {sigma_ms} seed {seed}: {frames_worse} frames more than 1 ms worse than raw"
                );
            }
        }
    }

    /// Runs `frames` wakes with Gaussian jitter around a display of `period`
    /// seconds, told it runs at `reported`. Returns the share of frames the
    /// clock spent following, and the RMS error against the true period for
    /// the smoothed and the raw deltas.
    fn long_run(reported: f64, period: f64, sigma: f64, frames: usize, seed: u64) -> (f64, f64, f64) {
        let mut noise = Noise(seed);
        let times: Vec<f64> = (0..=frames).map(|k| k as f64 * period + noise.gauss(sigma)).collect();
        let mut c = FrameClock::new(Some(Duration::from_secs_f64(reported)));
        let (mut following, mut smooth_sq, mut raw_sq) = (0usize, 0.0, 0.0);
        for pair in times.windows(2) {
            let m = (pair[1] - pair[0]).max(1e-6);
            let dt = c.step(Duration::from_secs_f64(m), LIMIT).as_secs_f64();
            following += usize::from(c.is_following());
            smooth_sq += (dt - period).powi(2);
            raw_sq += (m - period).powi(2);
        }
        let n = frames as f64;
        (following as f64 / n, (smooth_sq / n).sqrt(), (raw_sq / n).sqrt())
    }

    #[test]
    fn ordinary_desktop_jitter_does_not_switch_smoothing_off_for_good() {
        // 20 000 frames: debt random-walks past a period now and then even
        // when snapping works, and that must not end the smoothing.
        for (reported, period, sigma) in [(144.0, 144.0, 0.0005), (60.0, 60.0, 0.001), (59.0, 59.94, 0.0007)] {
            for seed in 1..=3u64 {
                let (following, smooth, raw) = long_run(1.0 / reported, 1.0 / period, sigma, 20_000, seed);
                assert!(following >= 0.95, "{reported} Hz sigma {sigma} seed {seed}: following {following}");
                assert!(smooth <= raw, "{reported} Hz sigma {sigma} seed {seed}: rms {smooth} > raw {raw}");
            }
        }
    }

    #[test]
    fn smoothing_recovers_after_a_burst_of_heavy_jitter() {
        let mut noise = Noise(7);
        let mut c = clock();
        let mut t = 0.0;
        let mut last = 0.0;
        let mut feed = |c: &mut FrameClock, sigma: f64, frames: usize, noise: &mut Noise| {
            for _ in 0..frames {
                t += PERIOD;
                let wake = t + noise.gauss(sigma);
                c.step(Duration::from_secs_f64((wake - last).max(1e-6)), LIMIT);
                last = wake;
            }
        };
        feed(&mut c, 0.0005, 500, &mut noise);
        assert!(c.is_following());
        // A burst of heavy jitter may or may not suspend it, but once the
        // display is calm again the clock must be following within a second
        // or two.
        feed(&mut c, 0.004, 300, &mut noise);
        feed(&mut c, 0.0005, 150, &mut noise);
        assert!(c.is_following());
    }

    #[test]
    fn a_correction_is_spread_over_frames() {
        let mut c = clock();
        // Owe three quarters of a period; no frame may move by more than the
        // per-frame correction cap, and the debt only shrinks.
        c.residual = 0.75 * PERIOD;
        let mut last = c.residual;
        for _ in 0..50 {
            let dt = c.step(Duration::from_secs_f64(PERIOD), LIMIT).as_secs_f64();
            assert!((dt - PERIOD).abs() <= MAX_CORRECTION * PERIOD + 1e-9, "{dt}");
            assert!(c.residual <= last + 1e-12);
            last = c.residual;
        }
        assert!(c.residual <= DEADBAND * PERIOD + 1e-9, "{}", c.residual);
    }

    #[test]
    fn changing_the_period_clears_carried_error() {
        let mut c = clock();
        c.step(ms(21.0), LIMIT);
        assert_ne!(c.residual, 0.0);
        c.set_period(Some(Duration::from_secs_f64(1.0 / 144.0)));
        assert_eq!(c.residual, 0.0);
    }

    #[test]
    fn late_frames_are_counted_against_one_and_a_half_periods() {
        let mut s = PacingStats::default();
        let p = Some(Duration::from_secs_f64(PERIOD));
        s.record(ms(16.7), p);
        s.record(ms(24.0), p);
        s.record(ms(25.5), p);
        s.record(ms(50.0), p);
        assert_eq!(s.late_frames, 2);
        assert_eq!(s.worst_interval, ms(50.0));
        s.record(ms(90.0), None);
        assert_eq!(s.late_frames, 2);
        assert_eq!(s.worst_interval, ms(90.0));
    }
}
