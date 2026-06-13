//! Spring physics — closed-form solution, no per-frame JS loop.
//!
//! Ported from Motion's `motion-dom/src/animation/generators/spring.ts`,
//! with flip-toolkit's named presets. The spring is sampled at a
//! fixed step to produce a `linear()` CSS easing string — see
//! [`crate::easing::sample_to_linear_easing`] — so the browser's
//! compositor runs the animation on the GPU with no per-frame JS.
//!
//! Three regimes, chosen by the dimensionless damping ratio
//! `ζ = damping / 2√(stiffness · mass)`:
//!
//! * `ζ < 1` — underdamped. Oscillates then settles.
//! * `ζ = 1` — critically damped. Fastest non-oscillating approach.
//! * `ζ > 1` — overdamped. Sluggish exponential approach.
//!
//! Each regime has a closed-form position-vs-time expression. We
//! never integrate numerically.

/// Spring configuration. Either specify physics directly
/// ([`Spring::physics`]) or describe the feel via
/// visual-duration + bounce ([`Spring::visual`]).
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
    /// Initial velocity in units-per-second. `0.0` starts from rest.
    pub velocity: f64,
    /// Speed below which the spring is considered at rest (per ms).
    pub rest_speed: f64,
    /// Distance from target below which the spring is considered at
    /// rest.
    pub rest_delta: f64,
}

impl Default for Spring {
    /// Motion's default — "gentle settle" that feels lively without
    /// wobbling.
    fn default() -> Self {
        Self {
            stiffness: 100.0,
            damping: 10.0,
            mass: 1.0,
            velocity: 0.0,
            rest_speed: 2.0,
            rest_delta: 0.5,
        }
    }
}

impl Spring {
    /// Build from raw physics. `mass = 1.0` is the usual choice.
    pub fn physics(stiffness: f64, damping: f64, mass: f64) -> Self {
        Self {
            stiffness,
            damping,
            mass,
            ..Self::default()
        }
    }

    /// Build from ergonomic visual parameters. `visual_duration` is
    /// the *perceived* animation time in seconds (typically 0.2–0.6);
    /// `bounce` is 0.0 (no overshoot) to 1.0 (maximum bounce).
    /// Default Motion tuning: `visual(0.3, 0.25)`.
    ///
    /// Internally resolves to stiffness/damping via
    /// Motion's formula: for a critically-damped equivalent the
    /// undamped angular frequency ω₀ = 2π / T, so k = ω₀²·m and
    /// c = 2·√(k·m)·ζ where ζ = 1 − bounce.
    pub fn visual(visual_duration_s: f64, bounce: f64) -> Self {
        let bounce = bounce.clamp(0.0, 0.99);
        let damping_ratio = (1.0 - bounce).clamp(0.05, 1.0);
        let mass = 1.0;
        // ω₀ derived so the 1-σ settle time matches visual_duration.
        // This mirrors Motion's `findSpring`'s numeric solve, but the
        // closed-form approximation below is within a few percent
        // for the visual-duration range authors use in practice.
        let omega = std::f64::consts::TAU / visual_duration_s.max(0.05);
        let stiffness = omega * omega * mass;
        let damping = 2.0 * (stiffness * mass).sqrt() * damping_ratio;
        Self {
            stiffness,
            damping,
            mass,
            velocity: 0.0,
            rest_speed: 2.0,
            rest_delta: 0.5,
        }
    }

    /// Set the initial velocity (units/second). Useful when a drag
    /// release hands off into a spring — pass the fling velocity so
    /// the rest animation continues the gesture's momentum.
    pub fn with_velocity(mut self, v: f64) -> Self {
        self.velocity = v;
        self
    }

    /// Sample the spring's normalized progress at time `t_ms` for a
    /// unit 0 → 1 animation. Used by the linear-easing sampler.
    pub fn at(&self, t_ms: f64) -> f64 {
        self.solve().position(t_ms)
    }

    /// Duration in ms at which the spring settles (velocity and
    /// position both within the rest thresholds). Capped at 20s.
    pub fn settle_duration_ms(&self) -> f64 {
        let solver = self.solve();
        let step_ms = 50.0;
        let mut t = 0.0;
        let max = 20_000.0;
        while t < max {
            let pos = solver.position(t);
            let vel_per_ms = solver.velocity(t) / 1000.0;
            if (1.0 - pos).abs() <= self.rest_delta / 100.0
                && vel_per_ms.abs() <= self.rest_speed / 1000.0
            {
                return t;
            }
            t += step_ms;
        }
        max
    }

    fn solve(&self) -> SpringSolver {
        let initial_delta = 1.0_f64; // we always normalize to origin=0, target=1
        // Motion convention: convert user's units/sec to internal
        // units/ms and negate. Positive user velocity then reads as
        // "faster initial movement toward target", which is what
        // authors expect when handing off drag momentum into a
        // spring. See `motion-dom/src/animation/generators/spring.ts`
        // line ~256 where the same conversion happens before the
        // A-coefficient is built.
        let initial_velocity = -self.velocity / 1000.0;
        let undamped_angular_freq = (self.stiffness / self.mass).sqrt() / 1000.0;
        let damping_ratio = self.damping / (2.0 * (self.stiffness * self.mass).sqrt());

        if (damping_ratio - 1.0).abs() < 1e-6 {
            SpringSolver::CriticallyDamped {
                target: 1.0,
                initial_delta,
                initial_velocity,
                omega: undamped_angular_freq,
            }
        } else if damping_ratio < 1.0 {
            let angular_freq = undamped_angular_freq * (1.0 - damping_ratio * damping_ratio).sqrt();
            let a = (initial_velocity + damping_ratio * undamped_angular_freq * initial_delta)
                / angular_freq;
            SpringSolver::Underdamped {
                target: 1.0,
                initial_delta,
                damping_ratio,
                omega: undamped_angular_freq,
                angular_freq,
                a,
            }
        } else {
            let damped_angular_freq =
                undamped_angular_freq * (damping_ratio * damping_ratio - 1.0).sqrt();
            SpringSolver::Overdamped {
                target: 1.0,
                initial_delta,
                initial_velocity,
                damping_ratio,
                omega: undamped_angular_freq,
                damped_angular_freq,
            }
        }
    }
}

/// Named presets borrowed from flip-toolkit (which borrowed them
/// from react-motion). These are the canonical "feel" names in the
/// React spring world, so authors coming from that ecosystem know
/// what to expect.
impl Spring {
    pub fn no_wobble() -> Self {
        Self::physics(200.0, 26.0, 1.0)
    }
    pub fn gentle() -> Self {
        Self::physics(120.0, 14.0, 1.0)
    }
    pub fn very_gentle() -> Self {
        Self::physics(130.0, 17.0, 1.0)
    }
    pub fn wobbly() -> Self {
        Self::physics(180.0, 12.0, 1.0)
    }
    pub fn stiff() -> Self {
        Self::physics(260.0, 26.0, 1.0)
    }
}

enum SpringSolver {
    Underdamped {
        target: f64,
        initial_delta: f64,
        damping_ratio: f64,
        omega: f64,
        angular_freq: f64,
        a: f64,
    },
    CriticallyDamped {
        target: f64,
        initial_delta: f64,
        initial_velocity: f64,
        omega: f64,
    },
    Overdamped {
        target: f64,
        initial_delta: f64,
        initial_velocity: f64,
        damping_ratio: f64,
        omega: f64,
        damped_angular_freq: f64,
    },
}

impl SpringSolver {
    fn position(&self, t_ms: f64) -> f64 {
        match *self {
            SpringSolver::Underdamped {
                target,
                initial_delta,
                damping_ratio,
                omega,
                angular_freq,
                a,
            } => {
                let envelope = (-damping_ratio * omega * t_ms).exp();
                target
                    - envelope
                        * (a * (angular_freq * t_ms).sin()
                            + initial_delta * (angular_freq * t_ms).cos())
            }
            SpringSolver::CriticallyDamped {
                target,
                initial_delta,
                initial_velocity,
                omega,
            } => {
                target
                    - (-omega * t_ms).exp()
                        * (initial_delta + (initial_velocity + omega * initial_delta) * t_ms)
            }
            SpringSolver::Overdamped {
                target,
                initial_delta,
                initial_velocity,
                damping_ratio,
                omega,
                damped_angular_freq,
            } => {
                let envelope = (-damping_ratio * omega * t_ms).exp();
                // Clamp the sinh/cosh argument to avoid Infinity.
                let freq_for_t = (damped_angular_freq * t_ms).min(300.0);
                target
                    - (envelope
                        * ((initial_velocity + damping_ratio * omega * initial_delta)
                            * freq_for_t.sinh()
                            + damped_angular_freq * initial_delta * freq_for_t.cosh()))
                        / damped_angular_freq
            }
        }
    }

    /// Velocity in units-per-second at time `t_ms`. Used by the
    /// settle predicate.
    fn velocity(&self, t_ms: f64) -> f64 {
        // Finite difference is simpler than deriving each analytic
        // form and plenty accurate for the rest-threshold check.
        let h = 0.5_f64; // ms
        let p1 = self.position(t_ms + h);
        let p0 = self.position((t_ms - h).max(0.0));
        (p1 - p0) / (2.0 * h) * 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_starts_at_zero_ends_near_one() {
        let s = Spring::default();
        assert!(s.at(0.0).abs() < 1e-9, "position at t=0 should be 0");
        let settle = s.settle_duration_ms();
        assert!(
            (s.at(settle) - 1.0).abs() < 0.01,
            "position at settle should be ~1 (got {})",
            s.at(settle)
        );
    }

    #[test]
    fn underdamped_overshoots_one() {
        // wobbly has a low damping ratio, so position should exceed
        // 1.0 at some point before settling.
        let s = Spring::wobbly();
        let mut max = 0.0_f64;
        let mut t = 0.0_f64;
        while t < 2000.0 {
            max = max.max(s.at(t));
            t += 5.0;
        }
        assert!(max > 1.0, "wobbly should overshoot target, got max {max}");
    }

    #[test]
    fn stiff_settles_faster_than_gentle() {
        let stiff = Spring::stiff().settle_duration_ms();
        let gentle = Spring::gentle().settle_duration_ms();
        assert!(
            stiff < gentle,
            "stiff ({stiff}ms) should settle before gentle ({gentle}ms)"
        );
    }

    #[test]
    fn no_wobble_never_overshoots() {
        let s = Spring::no_wobble();
        let mut t = 0.0_f64;
        while t < 3000.0 {
            assert!(
                s.at(t) <= 1.0 + 1e-3,
                "no_wobble overshot at t={t}: pos={}",
                s.at(t)
            );
            t += 5.0;
        }
    }

    #[test]
    fn visual_constructor_respects_bounce() {
        let no_bounce = Spring::visual(0.3, 0.0);
        let with_bounce = Spring::visual(0.3, 0.5);
        // Higher bounce → lower damping ratio → overshoot.
        let mut max_no = 0.0_f64;
        let mut max_yes = 0.0_f64;
        let mut t = 0.0_f64;
        while t < 1500.0 {
            max_no = max_no.max(no_bounce.at(t));
            max_yes = max_yes.max(with_bounce.at(t));
            t += 5.0;
        }
        assert!(
            max_yes > max_no,
            "bounce=0.5 should overshoot more than bounce=0 (got {max_yes} vs {max_no})"
        );
    }

    #[test]
    fn with_velocity_starts_moving() {
        let s = Spring::default().with_velocity(5.0);
        // With positive initial velocity, position at a small t
        // should be greater than the zero-velocity default.
        let base = Spring::default().at(10.0);
        let with = s.at(10.0);
        assert!(
            with > base,
            "positive initial velocity should produce larger position at t=10 (got {with} vs {base})"
        );
    }
}
