//! Per-tick health sampler + leak heuristic.
//!
//! The health panel wants a *history* of memory-pressure scalars
//! (listener count, effect count, scope count, dep count) even when
//! the panel isn't the active tab. Polling only during its own
//! render would lose samples; instead, [`sample_tick`] is called
//! from the devtools shell's render loop once per 200ms — the
//! panel reads from a ring of the last [`HISTORY`] samples when
//! it paints.
//!
//! Keeps ring buffers small (60 samples ≈ 12s window). Sparkline
//! renderer in `util` later; this file owns only the numbers.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::mount;
use crate::reactive;
use crate::scope::Scope;

pub(crate) const HISTORY: usize = 60;

#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Sample {
    pub listeners: usize,
    pub effects: usize,
    pub scopes: usize,
    pub deps: usize,
}

thread_local! {
    static SAMPLES: RefCell<VecDeque<Sample>> =
        RefCell::new(VecDeque::with_capacity(HISTORY));
}

/// Capture one sample. Called from the shell render loop — runs
/// even when the health panel isn't the active tab so history
/// fills silently.
pub(crate) fn sample_tick() {
    let (effects, deps) = reactive::stats();
    let sample = Sample {
        listeners: mount::listener_count(),
        effects,
        scopes: Scope::all().len(),
        deps,
    };
    SAMPLES.with(|s| {
        let mut v = s.borrow_mut();
        if v.len() == HISTORY {
            v.pop_front();
        }
        v.push_back(sample);
    });
}

/// Snapshot copy of the sample ring for rendering.
pub(crate) fn snapshot() -> Vec<Sample> {
    SAMPLES.with(|s| s.borrow().iter().copied().collect())
}

/// Heuristic leak detector: is every sample in the last-half of
/// the ring strictly greater than every sample in the first-half?
/// That's "only grew, never shrank" across a full ring window.
///
/// Returns `true` for any metric that's leaking; callers decide
/// which metrics to display-flag. Cheap: O(N) per metric, N ≤ 60.
pub(crate) fn monotonic_growth(samples: &[Sample]) -> Leaks {
    if samples.len() < HISTORY {
        // Not enough history — no judgment yet.
        return Leaks::default();
    }
    let mid = samples.len() / 2;
    let (head, tail) = samples.split_at(mid);
    Leaks {
        listeners: strictly_above(head, tail, |s| s.listeners),
        effects: strictly_above(head, tail, |s| s.effects),
        scopes: strictly_above(head, tail, |s| s.scopes),
        deps: strictly_above(head, tail, |s| s.deps),
    }
}

fn strictly_above<F: Fn(&Sample) -> usize>(head: &[Sample], tail: &[Sample], f: F) -> bool {
    if head.is_empty() || tail.is_empty() {
        return false;
    }
    let head_max = head.iter().map(&f).max().unwrap_or(0);
    let tail_min = tail.iter().map(&f).min().unwrap_or(0);
    // "Always above" — every tail sample > every head sample.
    tail_min > head_max
}

#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Leaks {
    pub listeners: bool,
    pub effects: bool,
    pub scopes: bool,
    pub deps: bool,
}

impl Leaks {
    pub(crate) fn any(&self) -> bool {
        self.listeners || self.effects || self.scopes || self.deps
    }
}

/// Render a tiny SVG sparkline for a series of `usize` samples.
/// `width` × `height` in CSS px; `color` is a CSS color string.
/// Baseline drawn at the series min; peaks at max. Empty series
/// returns an empty string so callers can do `.unwrap_or("")`.
pub(crate) fn sparkline_svg(values: &[usize], width: u32, height: u32, color: &str) -> String {
    if values.len() < 2 {
        return format!(
            "<svg width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"></svg>",
        );
    }
    let min = *values.iter().min().unwrap_or(&0);
    let max = *values.iter().max().unwrap_or(&0);
    let span = max.saturating_sub(min).max(1);
    let n = values.len();
    let dx = (width as f64) / ((n - 1).max(1) as f64);
    let mut points = String::with_capacity(n * 10);
    for (i, v) in values.iter().enumerate() {
        let x = (i as f64) * dx;
        // Invert Y so higher values paint upward.
        let y = (height as f64) - (((v - min) as f64 / span as f64) * (height as f64 - 2.0)) - 1.0;
        if i > 0 {
            points.push(' ');
        }
        points.push_str(&format!("{x:.1},{y:.1}"));
    }
    format!(
        "<svg width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" \
              preserveAspectRatio=\"none\">\
           <polyline points=\"{points}\" fill=\"none\" \
                     stroke=\"{color}\" stroke-width=\"1\"/>\
         </svg>",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(listeners: usize, effects: usize, scopes: usize, deps: usize) -> Sample {
        Sample {
            listeners,
            effects,
            scopes,
            deps,
        }
    }

    #[test]
    fn sparkline_of_empty_returns_empty_svg() {
        assert!(sparkline_svg(&[], 100, 20, "#fff").contains("</svg>"));
    }

    #[test]
    fn sparkline_of_single_value_paints_no_line() {
        let svg = sparkline_svg(&[5], 100, 20, "#fff");
        // Single-value series skips the polyline branch.
        assert!(!svg.contains("polyline"));
    }

    #[test]
    fn sparkline_uses_min_max_for_range() {
        let svg = sparkline_svg(&[10, 20, 30], 100, 20, "#fff");
        assert!(svg.contains("polyline"));
        assert!(svg.contains("stroke=\"#fff\""));
    }

    #[test]
    fn monotonic_growth_flags_strictly_increasing_tail() {
        let mut full: Vec<Sample> = Vec::with_capacity(HISTORY);
        for i in 0..HISTORY {
            full.push(s(i, 0, 0, 0));
        }
        let leaks = monotonic_growth(&full);
        assert!(leaks.listeners, "monotonic growth must flag leak");
        assert!(!leaks.effects);
    }

    #[test]
    fn monotonic_growth_ignores_steady_state() {
        let mut full: Vec<Sample> = Vec::with_capacity(HISTORY);
        for _ in 0..HISTORY {
            full.push(s(10, 10, 10, 10));
        }
        let leaks = monotonic_growth(&full);
        assert!(!leaks.any(), "flat series must not flag");
    }

    #[test]
    fn monotonic_growth_ignores_short_history() {
        let partial: Vec<Sample> = (0..5).map(|i| s(i, 0, 0, 0)).collect();
        let leaks = monotonic_growth(&partial);
        assert!(!leaks.any(), "short series must defer judgment");
    }
}
