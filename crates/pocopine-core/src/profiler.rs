//! Optional runtime profiling probes.
//!
//! Feature-gated (`mount-profiler`) so normal builds keep the
//! probes as zero-cost no-ops. Debug harnesses enable the
//! feature and flip `window.__POCOPINE_MOUNT_PROFILE = true`
//! around the action(s) they want to measure.
//!
//! The profiler captures four families of work:
//!
//! - [`mount`] — compiled `pp-for` row mount path
//!   (clone_template_body, node-path resolution, initial binding
//!   apply, listener install, DOM insertion).
//! - [`reconcile`] — `for_::run_keyed`'s effect closure (pool
//!   build, per-row iter, leaver drain, DOM reorder).
//! - [`state_sync`] — `Handle::update` / `Scope::invoke` phases
//!   (closure body, field-cache invalidate, trigger sweep).
//! - [`unmount`] — `walker::release_subtree` per-subtree teardown
//!   (effects release, listener release, scope removal).
//!
//! All four share the same enable flag + JSON envelope; one
//! [`report`] call emits a combined line tagged with the bench
//! action name. The bench harness wraps each measured action
//! with `reset()` / `report(action)`.

#[cfg(feature = "mount-profiler")]
use std::cell::RefCell;

#[cfg(feature = "mount-profiler")]
use js_sys::Reflect;
#[cfg(feature = "mount-profiler")]
use wasm_bindgen::JsValue;
#[cfg(feature = "mount-profiler")]
use web_sys::{console, window};

pub const ENABLE_FLAG: &str = "__POCOPINE_MOUNT_PROFILE";
pub const LOG_PREFIX: &str = "POCOPINE_MOUNT_PROFILE ";

#[cfg(feature = "mount-profiler")]
#[derive(Default, Clone)]
struct Metrics {
    // Action envelope.
    action_start_ms: f64,

    // ── mount phase ─────────────────────────────────────────
    clone_template_body_ms: f64,
    node_path_resolution_ms: f64,
    initial_binding_apply_ms: f64,
    listener_installation_ms: f64,
    dom_insertion_ms: f64,
    compiled_rows_mounted: u32,
    generic_rows_mounted: u32,

    // ── reconcile phase (for_::run_keyed effect) ────────────
    reconcile_total_ms: f64,
    reconcile_pool_build_ms: f64,
    reconcile_row_iter_ms: f64,
    reconcile_leaver_drain_ms: f64,
    reconcile_reorder_ms: f64,
    reconcile_runs: u32,

    // ── state-sync phase (Handle::update / Scope::invoke) ───
    state_sync_total_ms: f64,
    state_sync_closure_ms: f64,
    state_sync_invalidate_ms: f64,
    state_sync_trigger_ms: f64,
    state_sync_runs: u32,

    // ── unmount phase (release_subtree) ─────────────────────
    unmount_total_ms: f64,
    unmount_subtrees: u32,
}

#[cfg(feature = "mount-profiler")]
thread_local! {
    static METRICS: RefCell<Metrics> = RefCell::new(Metrics::default());
}

#[cfg(feature = "mount-profiler")]
fn now_ms() -> f64 {
    window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

#[cfg(feature = "mount-profiler")]
fn enabled_inner() -> bool {
    let Some(w) = window() else {
        return false;
    };
    Reflect::get(&w, &JsValue::from_str(ENABLE_FLAG))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(feature = "mount-profiler")]
fn pct(part: f64, total: f64) -> f64 {
    if total > 0.0 {
        part * 100.0 / total
    } else {
        0.0
    }
}

#[cfg(feature = "mount-profiler")]
pub fn enabled() -> bool {
    enabled_inner()
}

#[cfg(feature = "mount-profiler")]
pub fn reset() {
    if !enabled_inner() {
        return;
    }
    METRICS.with(|m| {
        *m.borrow_mut() = Metrics {
            action_start_ms: now_ms(),
            ..Metrics::default()
        };
    });
}

#[cfg(feature = "mount-profiler")]
pub fn start() -> Option<f64> {
    enabled_inner().then(now_ms)
}

#[cfg(feature = "mount-profiler")]
pub fn report(action: &str) {
    if !enabled_inner() {
        return;
    }
    let metrics = METRICS.with(|m| m.borrow().clone());
    let measured_mount_ms = metrics.clone_template_body_ms
        + metrics.node_path_resolution_ms
        + metrics.initial_binding_apply_ms
        + metrics.listener_installation_ms
        + metrics.dom_insertion_ms;
    let action_total_ms = now_ms() - metrics.action_start_ms;
    // Time the action spent that NONE of the instrumented phases
    // captured. Big residuals point at the next thing to
    // instrument.
    let unaccounted_ms = (action_total_ms
        - measured_mount_ms
        - metrics.reconcile_total_ms
        - metrics.state_sync_total_ms
        - metrics.unmount_total_ms)
        .max(0.0);
    let payload = serde_json::json!({
        "action": action,
        "action_total_ms": action_total_ms,
        // Mount phase.
        "measured_mount_ms": measured_mount_ms,
        "clone_template_body_ms": metrics.clone_template_body_ms,
        "clone_template_body_pct": pct(metrics.clone_template_body_ms, measured_mount_ms),
        "node_path_resolution_ms": metrics.node_path_resolution_ms,
        "node_path_resolution_pct": pct(metrics.node_path_resolution_ms, measured_mount_ms),
        "initial_binding_apply_ms": metrics.initial_binding_apply_ms,
        "initial_binding_apply_pct": pct(metrics.initial_binding_apply_ms, measured_mount_ms),
        "listener_installation_ms": metrics.listener_installation_ms,
        "listener_installation_pct": pct(metrics.listener_installation_ms, measured_mount_ms),
        "dom_insertion_ms": metrics.dom_insertion_ms,
        "dom_insertion_pct": pct(metrics.dom_insertion_ms, measured_mount_ms),
        "compiled_rows_mounted": metrics.compiled_rows_mounted,
        "generic_rows_mounted": metrics.generic_rows_mounted,
        // Reconcile phase.
        "reconcile_total_ms": metrics.reconcile_total_ms,
        "reconcile_pool_build_ms": metrics.reconcile_pool_build_ms,
        "reconcile_row_iter_ms": metrics.reconcile_row_iter_ms,
        "reconcile_leaver_drain_ms": metrics.reconcile_leaver_drain_ms,
        "reconcile_reorder_ms": metrics.reconcile_reorder_ms,
        "reconcile_runs": metrics.reconcile_runs,
        // State-sync phase.
        "state_sync_total_ms": metrics.state_sync_total_ms,
        "state_sync_closure_ms": metrics.state_sync_closure_ms,
        "state_sync_invalidate_ms": metrics.state_sync_invalidate_ms,
        "state_sync_trigger_ms": metrics.state_sync_trigger_ms,
        "state_sync_runs": metrics.state_sync_runs,
        // Unmount phase.
        "unmount_total_ms": metrics.unmount_total_ms,
        "unmount_subtrees": metrics.unmount_subtrees,
        // Unaccounted residual — the next thing to instrument if
        // it's a big fraction of action_total_ms.
        "unaccounted_ms": unaccounted_ms,
    });
    console::log_1(&JsValue::from_str(&format!("{LOG_PREFIX}{payload}")));
}

// ─── mount-phase API (RFC 054 row mount) ─────────────────────

#[cfg(feature = "mount-profiler")]
pub mod mount {
    use super::{enabled_inner, now_ms, METRICS};

    pub use super::{report as super_report, reset as super_reset};
    pub use super::{ENABLE_FLAG, LOG_PREFIX};

    pub fn enabled() -> bool {
        enabled_inner()
    }
    pub fn reset() {
        super::reset();
    }
    pub fn start() -> Option<f64> {
        enabled_inner().then(now_ms)
    }
    pub fn record_clone_template_body(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().clone_template_body_ms += now_ms() - start);
        }
    }
    pub fn record_node_path_resolution(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().node_path_resolution_ms += now_ms() - start);
        }
    }
    pub fn record_initial_binding_apply(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().initial_binding_apply_ms += now_ms() - start);
        }
    }
    pub fn record_listener_installation(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().listener_installation_ms += now_ms() - start);
        }
    }
    pub fn record_dom_insertion(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().dom_insertion_ms += now_ms() - start);
        }
    }
    pub fn record_compiled_row_mounted() {
        if enabled_inner() {
            METRICS.with(|m| m.borrow_mut().compiled_rows_mounted += 1);
        }
    }
    pub fn record_generic_row_mounted() {
        if enabled_inner() {
            METRICS.with(|m| m.borrow_mut().generic_rows_mounted += 1);
        }
    }
    pub fn report(action: &str) {
        super::report(action);
    }
}

// ─── reconcile-phase API (for_::run_keyed effect closure) ─────

#[cfg(feature = "mount-profiler")]
pub mod reconcile {
    use super::{enabled_inner, now_ms, METRICS};

    pub fn start() -> Option<f64> {
        enabled_inner().then(now_ms)
    }
    pub fn record_total(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| {
                let mut x = m.borrow_mut();
                x.reconcile_total_ms += now_ms() - start;
                x.reconcile_runs += 1;
            });
        }
    }
    pub fn record_pool_build(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().reconcile_pool_build_ms += now_ms() - start);
        }
    }
    pub fn record_row_iter(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().reconcile_row_iter_ms += now_ms() - start);
        }
    }
    pub fn record_leaver_drain(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().reconcile_leaver_drain_ms += now_ms() - start);
        }
    }
    pub fn record_reorder(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().reconcile_reorder_ms += now_ms() - start);
        }
    }
}

// ─── state-sync API (Handle::update / Scope::invoke) ──────────

#[cfg(feature = "mount-profiler")]
pub mod state_sync {
    use super::{enabled_inner, now_ms, METRICS};

    pub fn start() -> Option<f64> {
        enabled_inner().then(now_ms)
    }
    pub fn record_total(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| {
                let mut x = m.borrow_mut();
                x.state_sync_total_ms += now_ms() - start;
                x.state_sync_runs += 1;
            });
        }
    }
    pub fn record_closure(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().state_sync_closure_ms += now_ms() - start);
        }
    }
    pub fn record_invalidate(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().state_sync_invalidate_ms += now_ms() - start);
        }
    }
    pub fn record_trigger(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| m.borrow_mut().state_sync_trigger_ms += now_ms() - start);
        }
    }
}

// ─── unmount API (release_subtree) ────────────────────────────

#[cfg(feature = "mount-profiler")]
pub mod unmount {
    use super::{enabled_inner, now_ms, METRICS};

    pub fn start() -> Option<f64> {
        enabled_inner().then(now_ms)
    }
    pub fn record_total(start: Option<f64>) {
        if let Some(start) = start {
            METRICS.with(|m| {
                let mut x = m.borrow_mut();
                x.unmount_total_ms += now_ms() - start;
                x.unmount_subtrees += 1;
            });
        }
    }
}

// ─── no-op stubs (feature off) ────────────────────────────────

#[cfg(not(feature = "mount-profiler"))]
pub fn enabled() -> bool {
    false
}
#[cfg(not(feature = "mount-profiler"))]
pub fn reset() {}
#[cfg(not(feature = "mount-profiler"))]
pub fn start() -> Option<f64> {
    None
}
#[cfg(not(feature = "mount-profiler"))]
pub fn report(_: &str) {}

#[cfg(not(feature = "mount-profiler"))]
pub mod mount {
    pub use super::{ENABLE_FLAG, LOG_PREFIX};
    pub fn enabled() -> bool {
        false
    }
    pub fn reset() {}
    pub fn start() -> Option<f64> {
        None
    }
    pub fn record_clone_template_body(_: Option<f64>) {}
    pub fn record_node_path_resolution(_: Option<f64>) {}
    pub fn record_initial_binding_apply(_: Option<f64>) {}
    pub fn record_listener_installation(_: Option<f64>) {}
    pub fn record_dom_insertion(_: Option<f64>) {}
    pub fn record_compiled_row_mounted() {}
    pub fn record_generic_row_mounted() {}
    pub fn report(_: &str) {}
}

#[cfg(not(feature = "mount-profiler"))]
pub mod reconcile {
    pub fn start() -> Option<f64> {
        None
    }
    pub fn record_total(_: Option<f64>) {}
    pub fn record_pool_build(_: Option<f64>) {}
    pub fn record_row_iter(_: Option<f64>) {}
    pub fn record_leaver_drain(_: Option<f64>) {}
    pub fn record_reorder(_: Option<f64>) {}
}

#[cfg(not(feature = "mount-profiler"))]
pub mod state_sync {
    pub fn start() -> Option<f64> {
        None
    }
    pub fn record_total(_: Option<f64>) {}
    pub fn record_closure(_: Option<f64>) {}
    pub fn record_invalidate(_: Option<f64>) {}
    pub fn record_trigger(_: Option<f64>) {}
}

#[cfg(not(feature = "mount-profiler"))]
pub mod unmount {
    pub fn start() -> Option<f64> {
        None
    }
    pub fn record_total(_: Option<f64>) {}
}
