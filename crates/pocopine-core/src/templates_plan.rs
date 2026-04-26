//! RFC-058 Phase 2 — compiled template plans.
//!
//! Macro-emitted static descriptor for an entire compiled
//! template's bindings, listeners, refs, and deferred-init
//! entries. The runtime fast-path in [`crate::walker::mount_component`]
//! consumes the plan when one is registered for a component
//! tag, calling the cleanup-safe install helpers from RFC-058
//! Phase 1 directly instead of running the per-attribute
//! directive scan.
//!
//! Companion to [`crate::directives::for_plan`]:
//!
//! * `for_plan` — RFC-054 — keyed `pp-for` row bodies; per-row
//!   instance plan; reused across every row mount.
//! * `templates_plan` (this) — RFC-058 — whole-template plan;
//!   one entry per `#[component]`; runs once per mount.
//!
//! The two share `BindingKind` / `StaticBinding` / `StaticListener`
//! types from [`crate::directives::for_plan`] so the install
//! helpers don't need to switch on which compiler emitted the
//! entry.
//!
//! v1 envelope per RFC-057 §6 (deferred to RFC-058 Phase 2):
//!
//! * Eligible: native HTML elements only; `pp-text`, `pp-html`,
//!   `pp-show`, `pp-bind:<attr>` (HTML attrs, not child-component
//!   props), `pp-on:<event>` with the §6.1 supported modifier
//!   set, `pp-ref`, `pp-init` (deferred).
//! * Not eligible (attribute-preserved, walker-owned): every
//!   directive on or under a non-HTML-native tag, every
//!   directive on `pp-for` / `pp-if` / `pp-teleport` / `<slot>`
//!   subtrees, `pp-model`, `pp-route`, listeners with
//!   unsupported modifiers.
//! * Stripped attributes are owned by the plan and **fail fast**
//!   on framework bugs; attribute-preserved fallbacks degrade
//!   silently to the runtime walker as today.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::directives::for_plan::{StaticBinding, StaticInit, StaticListener, StaticRef};

// ─── macro-emitted static shape ─────────────────────────────────

/// Static-lifetime template plan emitted by `#[component]`. The
/// macro emits one `&'static StaticTemplatePlan` per component
/// alongside the existing `register_template` call:
///
/// ```ignore
/// pub static __POC_TEMPLATE_PLAN_<COMPONENT>: StaticTemplatePlan =
///     StaticTemplatePlan {
///         bindings: &[ /* … */ ],
///         listeners: &[ /* … */ ],
///         inits: &[ /* … */ ],
///         refs: &[ /* … */ ],
///     };
/// ```
#[doc(hidden)]
pub struct StaticTemplatePlan {
    pub bindings: &'static [StaticBinding],
    pub listeners: &'static [StaticListener],
    pub inits: &'static [StaticInit],
    pub refs: &'static [StaticRef],
}

// ─── registry ────────────────────────────────────────────────────

thread_local! {
    static TEMPLATE_PLANS: RefCell<HashMap<String, &'static StaticTemplatePlan>> =
        RefCell::new(HashMap::new());

    /// Counter for fail-fast-but-recover events in release builds
    /// — incremented when an `apply_static_plan` install fails
    /// (`node_path` doesn't resolve, `expr_src` doesn't parse).
    /// Tests assert this stays zero across mount/unmount cycles
    /// for templates the macro stripped attributes from. Public
    /// reader: [`plan_failure_count`].
    static PLAN_FAILURES: Cell<u32> = const { Cell::new(0) };
}

/// Register a template plan against a component tag. Called by
/// macro-emitted code immediately after `register_template` for
/// every `#[component]` whose template has at least one
/// plan-eligible directive (RFC-058 Phase 2 §6 envelope).
///
/// Idempotent — repeat calls overwrite the prior entry.
pub fn register_template_plan(tag: &str, plan: &'static StaticTemplatePlan) {
    TEMPLATE_PLANS.with(|registry| {
        registry.borrow_mut().insert(tag.to_string(), plan);
    });
}

/// Look up a registered template plan by component tag. Returns
/// `None` for components the macro chose not to plan (e.g.
/// templates entirely inside non-HTML-native subtrees, or
/// pre-RFC-058 components compiled before the plan emitter
/// existed).
pub fn template_plan_for(tag: &str) -> Option<&'static StaticTemplatePlan> {
    TEMPLATE_PLANS.with(|registry| registry.borrow().get(tag).copied())
}

/// Cumulative count of plan-install failures observed since
/// process start. Increments via [`record_plan_failure`] when an
/// `apply_static_plan` entry's `node_path` doesn't resolve to a
/// live DOM node, or its `expr_src` doesn't parse — both are
/// framework bugs (the macro stripped a directive whose plan
/// entry can't deliver). Tests can assert this stays zero
/// across mount/unmount cycles for compiled components.
pub fn plan_failure_count() -> u32 {
    PLAN_FAILURES.with(|c| c.get())
}

/// Reset the plan-failure counter. Tests call this between
/// cases so a prior test's failure doesn't leak into the next
/// case's assertion. Production code should never call this.
#[doc(hidden)]
pub fn reset_plan_failure_count() {
    PLAN_FAILURES.with(|c| c.set(0));
}

/// Record one plan-install failure. Called from
/// [`crate::walker`] / future `apply_static_plan` when an
/// install entry can't deliver. In debug builds this also
/// panics with a message naming the template + entry; release
/// builds log to `console.error` and continue (the surrounding
/// mount keeps going so a single misclassification doesn't
/// take down the page).
#[doc(hidden)]
pub fn record_plan_failure() {
    PLAN_FAILURES.with(|c| c.set(c.get().saturating_add(1)));
}
