//! Idle behaviour: picking which animation a resting character plays next.
//!
//! Spec §12 asks for a default idle and random idle selection. Randomness here
//! is a seeded xorshift rather than the system RNG so that a recorded seed
//! replays exactly — visual regression tests and bug reports both need that.

use a2d_core::AnimationInfo;

/// A small deterministic PRNG.
///
/// xorshift64* — good enough for choosing between a handful of animations, and
/// reproducible across platforms, which `rand`'s thread RNG is not.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is a fixed point for xorshift, so nudge it away.
        Rng { state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed } }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform value in `0..n`. Returns 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// Uniform float in `0.0..1.0`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform float in `low..=high`.
    pub fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }
}

/// Picks idle animations for a resting character.
#[derive(Debug, Clone)]
pub struct IdleDirector {
    /// The animation played when nothing else is happening.
    default_idle: Option<String>,
    /// Occasional variations, played once before returning to the default.
    variations: Vec<String>,
    /// Seconds between variation attempts.
    pub interval: (f32, f32),
    elapsed: f32,
    next_at: f32,
    rng: Rng,
}

impl IdleDirector {
    /// Chooses a default idle and variations from an animation list.
    ///
    /// Anything whose name contains `idle` counts; the shortest such name is
    /// treated as the default and the rest become variations, which matches how
    /// rigs are normally named (`idle`, `idle_02`, `idle_blink`).
    pub fn from_animations(animations: &[AnimationInfo], seed: u64) -> Self {
        let mut idles: Vec<&AnimationInfo> =
            animations.iter().filter(|a| a.name.to_ascii_lowercase().contains("idle")).collect();
        idles.sort_by_key(|a| (a.name.len(), a.name.clone()));

        let default_idle = idles.first().map(|a| a.name.clone()).or_else(|| animations.first().map(|a| a.name.clone()));
        let variations = idles.iter().skip(1).map(|a| a.name.clone()).collect();

        let mut director = IdleDirector {
            default_idle,
            variations,
            interval: (8.0, 20.0),
            elapsed: 0.0,
            next_at: 0.0,
            rng: Rng::new(seed),
        };
        director.schedule_next();
        director
    }

    pub fn default_idle(&self) -> Option<&str> {
        self.default_idle.as_deref()
    }

    pub fn variations(&self) -> &[String] {
        &self.variations
    }

    fn schedule_next(&mut self) {
        self.elapsed = 0.0;
        self.next_at = self.rng.range(self.interval.0, self.interval.1);
    }

    /// Advances the idle timer. Returns a variation to play when one is due.
    ///
    /// The caller is expected to play it once, queued back into the default
    /// idle; this type only decides *what* and *when*.
    pub fn update(&mut self, dt: f32) -> Option<String> {
        if self.variations.is_empty() {
            return None;
        }
        self.elapsed += dt;
        if self.elapsed < self.next_at {
            return None;
        }
        let pick = self.rng.below(self.variations.len());
        let chosen = self.variations[pick].clone();
        self.schedule_next();
        Some(chosen)
    }

    /// Restarts the timer, e.g. after the user interacts with the character.
    pub fn reset(&mut self) {
        self.schedule_next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animations(names: &[&str]) -> Vec<AnimationInfo> {
        names.iter().map(|n| AnimationInfo { name: (*n).to_string(), duration: 1.0 }).collect()
    }

    #[test]
    fn the_same_seed_produces_the_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn a_zero_seed_does_not_get_stuck() {
        let mut rng = Rng::new(0);
        let first = rng.next_u64();
        assert_ne!(first, 0);
        assert_ne!(rng.next_u64(), first);
    }

    #[test]
    fn below_stays_in_range_and_handles_zero() {
        let mut rng = Rng::new(7);
        assert_eq!(rng.below(0), 0);
        for _ in 0..500 {
            assert!(rng.below(5) < 5);
        }
    }

    #[test]
    fn unit_stays_in_the_unit_interval() {
        let mut rng = Rng::new(9);
        for _ in 0..1000 {
            let v = rng.unit();
            assert!((0.0..1.0).contains(&v), "{v}");
        }
    }

    #[test]
    fn range_stays_within_its_bounds() {
        let mut rng = Rng::new(3);
        for _ in 0..1000 {
            let v = rng.range(5.0, 9.0);
            assert!((5.0..=9.0).contains(&v), "{v}");
        }
    }

    #[test]
    fn the_shortest_idle_name_becomes_the_default() {
        let d = IdleDirector::from_animations(&animations(&["idle_02", "idle", "walk", "idle_blink"]), 1);
        assert_eq!(d.default_idle(), Some("idle"));
        let mut vars = d.variations().to_vec();
        vars.sort();
        assert_eq!(vars, vec!["idle_02".to_string(), "idle_blink".to_string()]);
    }

    #[test]
    fn a_rig_with_no_idle_falls_back_to_the_first_animation() {
        let d = IdleDirector::from_animations(&animations(&["stand", "walk"]), 1);
        assert_eq!(d.default_idle(), Some("stand"));
        assert!(d.variations().is_empty());
    }

    #[test]
    fn a_rig_with_no_animations_has_no_default() {
        let d = IdleDirector::from_animations(&[], 1);
        assert_eq!(d.default_idle(), None);
    }

    #[test]
    fn no_variation_fires_before_the_interval_elapses() {
        let mut d = IdleDirector::from_animations(&animations(&["idle", "idle_02"]), 1);
        d.interval = (5.0, 5.0);
        d.reset();
        for _ in 0..49 {
            assert_eq!(d.update(0.1), None);
        }
    }

    #[test]
    fn a_variation_fires_once_the_interval_elapses() {
        let mut d = IdleDirector::from_animations(&animations(&["idle", "idle_02"]), 1);
        d.interval = (1.0, 1.0);
        d.reset();
        let mut fired = None;
        for _ in 0..20 {
            if let Some(name) = d.update(0.1) {
                fired = Some(name);
                break;
            }
        }
        assert_eq!(fired.as_deref(), Some("idle_02"));
    }

    #[test]
    fn a_rig_with_only_one_idle_never_fires_a_variation() {
        let mut d = IdleDirector::from_animations(&animations(&["idle"]), 1);
        d.interval = (0.1, 0.1);
        d.reset();
        for _ in 0..100 {
            assert_eq!(d.update(1.0), None);
        }
    }

    #[test]
    fn variation_choice_is_reproducible_for_a_seed() {
        let run = || {
            let mut d = IdleDirector::from_animations(&animations(&["idle", "idle_a", "idle_b", "idle_c"]), 99);
            d.interval = (1.0, 2.0);
            d.reset();
            let mut picks = Vec::new();
            for _ in 0..300 {
                if let Some(name) = d.update(0.1) {
                    picks.push(name);
                }
            }
            picks
        };
        let first = run();
        assert!(!first.is_empty(), "some variations should have fired");
        assert_eq!(first, run());
    }
}
