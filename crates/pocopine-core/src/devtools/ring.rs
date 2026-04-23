//! Bounded timeline ring buffer.
//!
//! Stores the last [`CAP`] `TimelineEvent`s (effect runs + handler
//! invocations). Wraps via `pop_front` when full — older events drop
//! silently. A monotonic `seq` counter ticks once per push; panels
//! use `(len, last_seq)` as their fingerprint so the render loop
//! doesn't rewrite HTML between ticks when nothing new arrived.
//!
//! wasm is single-threaded so the thread-local RefCell is fine;
//! no lock contention path, no cross-thread tearing.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

use crate::reactive::{EffectId, ScopeId};

/// Max events retained. Tuned to a size that's useful for human
/// inspection and still small enough that scanning the whole buffer
/// per render tick is cheap. Raise once the timeline panel supports
/// pagination.
pub(crate) const CAP: usize = 200;

/// A single observed event. `seq` is globally monotonic across the
/// whole buffer — panels sort by it or use it to detect "have I
/// seen this event yet?" without equality on the enum.
#[derive(Clone, Debug)]
#[allow(dead_code)] // consumed by PR C's timeline panel
pub enum TimelineEvent {
    EffectRun {
        id: EffectId,
        scope: Option<ScopeId>,
        dur_us: u32,
        t_ms: f64,
        seq: u64,
    },
    Handler {
        scope: ScopeId,
        name: String,
        args_summary: String,
        dur_us: u32,
        t_ms: f64,
        seq: u64,
    },
}

impl TimelineEvent {
    pub fn seq(&self) -> u64 {
        match self {
            TimelineEvent::EffectRun { seq, .. } => *seq,
            TimelineEvent::Handler { seq, .. } => *seq,
        }
    }
}

thread_local! {
    static TIMELINE: RefCell<VecDeque<TimelineEvent>> =
        RefCell::new(VecDeque::with_capacity(CAP));

    /// Monotonic push counter. Never reset, never wraps in practice —
    /// 2^64 events at 1kHz is billions of years.
    static SEQ: Cell<u64> = const { Cell::new(0) };
}

/// Push an effect-run event. Called by the devtools default
/// `on_effect_run` handler registered at install time.
pub fn push_effect_run(id: EffectId, scope: Option<ScopeId>, dur: Duration) {
    let seq = SEQ.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    let dur_us = dur.as_micros().min(u32::MAX as u128) as u32;
    let t_ms = now_ms();
    push(TimelineEvent::EffectRun {
        id,
        scope,
        dur_us,
        t_ms,
        seq,
    });
}

/// Push a handler-invocation event. `args_summary` is the lazy
/// caller-provided stringified args — the handler can use
/// `util::stringify` or a cheaper shape, no cost is paid here.
pub fn push_handler(scope: ScopeId, name: &str, args_summary: String, dur: Duration) {
    let seq = SEQ.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    let dur_us = dur.as_micros().min(u32::MAX as u128) as u32;
    let t_ms = now_ms();
    push(TimelineEvent::Handler {
        scope,
        name: name.to_string(),
        args_summary,
        dur_us,
        t_ms,
        seq,
    });
}

fn push(ev: TimelineEvent) {
    TIMELINE.with(|t| {
        let mut buf = t.borrow_mut();
        if buf.len() == CAP {
            buf.pop_front();
        }
        buf.push_back(ev);
    });
}

/// Clone out a snapshot of the current events for rendering. The
/// panel holds the clone for a single render pass — cheap because
/// `TimelineEvent: Clone` is just a few strings + scalars.
#[allow(dead_code)] // used by PR C's timeline panel
pub fn snapshot() -> Vec<TimelineEvent> {
    TIMELINE.with(|t| t.borrow().iter().cloned().collect())
}

/// Number of events currently retained (≤ [`CAP`]).
#[allow(dead_code)] // used by PR C's fingerprint + wasm tests
pub fn len() -> usize {
    TIMELINE.with(|t| t.borrow().len())
}

/// Seq number of the most-recently-pushed event, or `0` when the
/// buffer is empty. Together with `len()` forms a cheap fingerprint.
#[allow(dead_code)] // used by PR C's fingerprint + wasm tests
pub fn last_seq() -> u64 {
    TIMELINE.with(|t| t.borrow().back().map(TimelineEvent::seq).unwrap_or(0))
}

/// Empty the buffer. Exposed publicly so integration tests can
/// start from a clean slate; production code has no reason to drop
/// the timeline manually.
#[doc(hidden)]
pub fn _clear() {
    TIMELINE.with(|t| t.borrow_mut().clear());
    SEQ.with(|c| c.set(0));
}

#[cfg(test)]
pub(crate) fn clear() {
    _clear();
}

fn now_ms() -> f64 {
    now_ms_for_scope()
}

/// Same timer as [`now_ms`] but exposed at crate-visibility so
/// `scope::invoke` (and any other non-reactive callsite) can bracket
/// its body with one consistent clock source. Native tests stub to
/// `0.0` because `web_sys::window()` panics off wasm.
pub fn now_ms_for_scope() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

// ── tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_appends_and_len_grows() {
        clear();
        push_effect_run(EffectId(1), None, Duration::from_micros(10));
        push_effect_run(EffectId(2), None, Duration::from_micros(20));
        assert_eq!(len(), 2);
    }

    #[test]
    fn seq_increments_monotonically() {
        clear();
        push_effect_run(EffectId(1), None, Duration::ZERO);
        let first = last_seq();
        push_effect_run(EffectId(2), None, Duration::ZERO);
        let second = last_seq();
        assert!(second > first, "seq must be monotonic");
    }

    #[test]
    fn ring_caps_at_cap_and_drops_oldest() {
        clear();
        for i in 0..(CAP + 50) {
            push_effect_run(EffectId(i as u64), None, Duration::ZERO);
        }
        assert_eq!(len(), CAP, "ring must cap at CAP");
        // Oldest retained should be seq = 51 (we dropped the first 50).
        let snap = snapshot();
        let first_seq = snap.first().unwrap().seq();
        let last_seq_val = snap.last().unwrap().seq();
        assert_eq!(last_seq_val - first_seq, (CAP as u64) - 1);
    }

    #[test]
    fn handler_event_carries_args_summary() {
        clear();
        push_handler(
            ScopeId(7),
            "on_click",
            "[MouseEvent]".into(),
            Duration::from_micros(42),
        );
        let snap = snapshot();
        match snap.first().unwrap() {
            TimelineEvent::Handler {
                scope,
                name,
                args_summary,
                dur_us,
                ..
            } => {
                assert_eq!(*scope, ScopeId(7));
                assert_eq!(name, "on_click");
                assert_eq!(args_summary, "[MouseEvent]");
                assert_eq!(*dur_us, 42);
            }
            _ => panic!("expected Handler variant"),
        }
    }
}
