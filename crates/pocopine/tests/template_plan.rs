//! RFC-058 Phase 2 — compiled-view evidence tests.
//!
//! Phase 2 ships a macro-emitted `&'static StaticTemplatePlan`
//! per plan-eligible component plus specialized install entries
//! applied without a recursive directive walk (the runtime
//! walker is gone — RFC-058 Phase 6.5). This file pins the
//! parts of the §6 envelope that are easy to lose silently.
//!
//! Plan-eligible templates register a plan and the registry
//! survives a mount/unmount cycle without the fail-fast counter
//! ticking. A planned `pp-text` whose evaluated value contains
//! literal braces stays byte-exact — the plan owns the
//! element's text and no runtime interpolation pass exists to
//! re-scan it.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test template_plan`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine_core::templates_plan::{
    plan_failure_count, reset_plan_failure_count, template_plan_for,
};
use serde::{Deserialize, Serialize};

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

wasm_bindgen_test_configure!(run_in_browser);

// ─── fixtures ────────────────────────────────────────────────────

/// Minimal plan-eligible component: one `pp-text` binding on a
/// native HTML element. Drives test #1 (plan registered + no
/// fail-fast) and #2 (braces in the bound value stay literal).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanTextEcho.html")]
struct PlanTextEcho {
    message: String,
}

#[handlers]
impl PlanTextEcho {
    pub fn on_setup(&mut self) {
        // A value with literal `{...}` braces — the planned
        // `pp-text` write must keep them byte-exact (no
        // interpolation pass may re-scan plan-owned text).
        self.message = "value: {count} ok".into();
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Rfc064ExprNested {
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rfc064-expr-host",
    template_inline = r#"<div>
        <span class="r64-ident" pp-text="label"></span>
        <span class="r64-field" pp-text="nested.label"></span>
        <span class="r64-literal" pp-text="'literal'"></span>
        <span class="r64-not" pp-text="!hidden"></span>
        <span class="r64-compare" pp-text="count > 2"></span>
        <span class="r64-bool" pp-text="ready && count > 2"></span>
        <span class="r64-fallback" pp-text="count > 2 ? 'yes' : 'no'"></span>
    </div>"#
)]
struct Rfc064ExprHost {
    label: String,
    nested: Rfc064ExprNested,
    hidden: bool,
    count: u32,
    ready: bool,
}

#[handlers]
impl Rfc064ExprHost {
    pub fn on_setup(&mut self) {
        self.label = "identifier".into();
        self.nested = Rfc064ExprNested {
            label: "field".into(),
        };
        self.hidden = false;
        self.count = 3;
        self.ready = true;
    }
}

/// Template whose only plan-relevant content is a
/// `<template pp-for>` row. With §6.2 layering, the row-plan
/// analyser still owns the row body, but the template-plan
/// classifier emits no template plan because there's no
/// eligible directive *outside* the pp-for to plan.
#[derive(Clone, Default, Serialize, Deserialize)]
struct PflRow {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanForLoop.html")]
struct PlanForLoop {
    rows: Vec<PflRow>,
}

#[handlers]
impl PlanForLoop {
    pub fn on_setup(&mut self) {
        self.rows = vec![PflRow {
            id: 1,
            label: "alpha".into(),
        }];
    }
}

/// RFC-058 §6.2 layering — template that mixes a plan-eligible
/// `pp-text` (outside `pp-for`) with an RFC-054 keyed row plan.
/// Both compilers must run: the template plan is registered for
/// the title binding, AND the row-plan registry resolves the
/// keyed list — proving the `data-pp-row-plan` attribute
/// survives the template-plan rewrite.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanForMixed.html")]
struct PlanForMixed {
    title: String,
    rows: Vec<PflRow>,
}

#[handlers]
impl PlanForMixed {
    pub fn on_setup(&mut self) {
        self.title = "mixed-title".into();
        self.rows = vec![
            PflRow {
                id: 1,
                label: "alpha".into(),
            },
            PflRow {
                id: 2,
                label: "beta".into(),
            },
        ];
    }
}

/// RFC-058 Phase 3 — leaf child component the parent below
/// nests. Plan-eligible (one `pp-text` on a native tag), so it
/// itself registers a template plan; the parent's plan
/// references it via a `StaticChildMount` entry.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanChildLeaf.html")]
struct PlanChildLeaf {
    caption: String,
}

#[handlers]
impl PlanChildLeaf {
    pub fn on_setup(&mut self) {
        self.caption = "leaf-mounted".into();
    }
}

/// Parent whose template contains a `<plan-child-leaf>` tag.
/// Drives the Phase 3.4 evidence — the parent's plan must
/// carry exactly one `StaticChildMount` entry, and mounting
/// the parent must mount the leaf without us reaching for the
/// mount's auto-discovery path.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanChildHost.html")]
struct PlanChildHost {}

#[handlers]
impl PlanChildHost {}

/// RFC-058 Phase 3.5a — minimal `<slot>`-bearing child for
/// the slot-fragment evidence test. The child's template stamps
/// a `<slot>`; the test mounts the child via
/// `mount_child_component_with_slots` with a hand-built
/// `SlotSet` whose default fragment writes a sentinel into the
/// `DocumentFragment` buffer. `materialize_slot` must invoke
/// the fragment (not the legacy capture path) because the test
/// passes no slot content via the host's children.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotChild.html")]
struct PlanSlotChild {}

#[handlers]
impl PlanSlotChild {}

/// RFC-058 Phase 3.5b — parent whose template wraps
/// `<plan-slot-child>` around static slot content. The
/// classifier must lift the children into a fragment function
/// the macro emits inside the parent's `register()`; the
/// parent's plan references that fragment via a
/// `StaticSlotFragment` entry; the runtime applier passes the
/// `SlotSet` through `mount_child_component_with_slots`; and
/// the mount's `materialize_slot` invokes the fragment
/// instead of running the legacy capture/replay path. End-to-
/// end macro-driven slot rendering with no mount
/// auto-discovery for the slot subtree.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotHostStatic.html")]
struct PlanSlotHostStatic {}

#[handlers]
impl PlanSlotHostStatic {}

/// RFC-058 Phase 3.5c — parent whose slot content carries
/// `pp-text` + `@click` against the parent scope. The
/// classifier graduates the subtree from "static-only" to
/// "dynamic" eligibility, emits a `stamp_dynamic_slot`-based
/// fragment that installs both directives via the Phase 1
/// helpers against the parent proxy, and the runtime
/// `materialize_slot` invokes it instead of the legacy
/// capture/replay path.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotDynamicHost.html")]
struct PlanSlotDynamicHost {
    title: String,
    count: u32,
}

#[handlers]
impl PlanSlotDynamicHost {
    pub fn on_setup(&mut self) {
        self.title = "initial".into();
    }
    pub fn bump(&mut self) {
        self.count += 1;
        self.title = format!("count-{}", self.count);
    }
}

/// Slot content that is *bare* `{{ }}` interpolation (no wrapping
/// element, no pp-text). The classifier scans top-level slot text
/// nodes and emits a dynamic fragment whose interp installs against
/// the author (parent) scope — previously this rendered as raw
/// braces (the AgenKitty `<pine-avatar-fallback>{{ initials }}</…>`
/// finding).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotInterpHost.html")]
struct PlanSlotInterpHost {
    label: String,
}

#[handlers]
impl PlanSlotInterpHost {
    pub fn on_setup(&mut self) {
        self.label = "first".into();
    }
    pub fn bump(&mut self) {
        self.label = "second".into();
    }
}

/// A compound component: its own `<slot>` outlet sits INSIDE a child
/// component's slot content (`<plan-slot-child><slot></slot></plan-slot-child>`)
/// — the AkContextMenu shape. The consumer's projected content must traverse
/// the child boundary and land inside the child's shell, and the sibling
/// `:label`-bound child (the `pine-icon :name` shape) must bind against the
/// author's scope — one rejected `<slot>` used to poison the whole fragment
/// tree, leaving the binding a raw attribute.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanCompoundHost.html")]
struct PlanCompoundHost {
    badge: String,
}

#[handlers]
impl PlanCompoundHost {
    pub fn on_setup(&mut self) {
        self.badge = "badge-ok".into();
    }
}

/// A child whose own `<slot>` outlet is DEFERRED inside a `pp-if` body (the
/// dropdown-portal shape): the outlet only exists once `open` flips, so the
/// slot content handed to it by a compound host must materialize LATE —
/// long after everyone mounted.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanDeferredSlotChild.html")]
struct PlanDeferredSlotChild {
    open: bool,
}

#[handlers]
impl PlanDeferredSlotChild {
    pub fn open_it(&mut self) {
        self.open = true;
    }
}

/// Compound host over the deferred child: its outlet rides the child's slot
/// content into a pp-if body two boundaries deep.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanCompoundDeferredHost.html")]
struct PlanCompoundDeferredHost {}

#[handlers]
impl PlanCompoundDeferredHost {}

/// The full dropdown-portal shape: the child's `<slot>` lives in a
/// `pp-if` + `pp-teleport="body"` template, so the projected content must
/// materialize late AND land teleported outside the host tree.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanTeleportSlotChild.html")]
struct PlanTeleportSlotChild {
    open: bool,
}

#[handlers]
impl PlanTeleportSlotChild {
    pub fn open_it(&mut self) {
        self.open = true;
    }
    pub fn close_it(&mut self) {
        self.open = false;
    }
}

/// Compound host over the teleporting child.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanCompoundTeleportHost.html")]
struct PlanCompoundTeleportHost {}

#[handlers]
impl PlanCompoundTeleportHost {}

/// A keyed pp-for whose rows each host a teleporting child (the AkTreeRow
/// shape): opening the row's portal puts a clone at `<body>`; deleting the
/// row must reap that clone. Row teardown is table-driven (it never walks
/// the row's DOM), so the per-element teleport stash used to be skipped
/// entirely — orphaning the panel at the viewport corner.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanRowPortalHost.html")]
struct PlanRowPortalHost {
    items: Vec<String>,
}

#[handlers]
impl PlanRowPortalHost {
    pub fn on_setup(&mut self) {
        self.items = vec!["a".to_string(), "b".to_string()];
    }
    pub fn remove_first(&mut self) {
        if !self.items.is_empty() {
            self.items.remove(0);
        }
    }
}

/// Child used by the compiled-finalize regression test. Its
/// `on_mount` write proves a lifted fragment containing a child
/// component can finish lifecycle without falling back to the
/// recursive mount.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanLifecycleLeaf.html")]
struct PlanLifecycleLeaf {
    caption: String,
}

#[handlers]
impl PlanLifecycleLeaf {
    pub fn on_mount(&mut self) {
        self.caption = "mounted-without-walk".into();
    }
}

/// Host whose pp-if body root is a child component. This is the
/// smallest shape that used to require `walk_compiled_fallback`
/// only for post-order lifecycle after generated child mounting.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfChildHost.html")]
struct PlanIfChildHost {
    open: bool,
}

#[handlers]
impl PlanIfChildHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// Component-host directive regression fixture. The child deliberately owns a
/// default slot so the parent exercises the exact
/// `<template pp-if><child>content</child></template>` shape.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-component-directive-child",
    template_inline = r#"<button class="pcdc-button"><slot></slot></button>"#
)]
struct PlanComponentDirectiveChild {}

#[handlers]
impl PlanComponentDirectiveChild {}

/// Covers structural directives whose target/body root is a component host:
/// a direct custom-element root under `pp-if`, including projected content and
/// a parent listener, plus `pp-show` directly on a `display: contents` host.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-component-directive-host",
    template_inline = r#"<div class="pcdh-root">
        <button class="pcdh-toggle-if" @click="toggle_open">toggle branch</button>
        <button class="pcdh-toggle-show" @click="toggle_visible">toggle visibility</button>
        <span class="pcdh-clicks" pp-text="clicks"></span>
        <template pp-if="open">
            <plan-component-directive-child data-pcdh="if" class="pcdh-if-child" @click="record_click">
                Delete
            </plan-component-directive-child>
        </template>
        <plan-component-directive-child data-pcdh="show" class="pcdh-show-child" pp-show="visible">
            Shown
        </plan-component-directive-child>
    </div>"#
)]
struct PlanComponentDirectiveHost {
    open: bool,
    visible: bool,
    clicks: u32,
}

#[handlers]
impl PlanComponentDirectiveHost {
    pub fn toggle_open(&mut self) {
        self.open = !self.open;
    }

    pub fn toggle_visible(&mut self) {
        self.visible = !self.visible;
    }

    pub fn record_click(&mut self) {
        self.clicks += 1;
    }
}

/// Regression for inline-style ownership shared by `pp-show` and `:style`.
/// Both authored attribute orders are present because the directive effects
/// run immediately as the compiled plan installs them.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-show-style-host",
    template_inline = r#"<div class="pssh-root">
        <button class="pssh-toggle" @click="toggle_visible">toggle visibility</button>
        <button class="pssh-update" @click="update_styles">update styles</button>
        <div class="pssh-show-first" pp-show="visible" :style="style_value"></div>
        <div class="pssh-style-first" :style="style_value" pp-show="visible"></div>
        <div class="pssh-bound-display" pp-show="visible" :style="display_style"></div>
    </div>"#
)]
struct PlanShowStyleHost {
    visible: bool,
    alternate: bool,
    style_value: String,
    display_style: String,
}

#[handlers]
impl PlanShowStyleHost {
    pub fn on_setup(&mut self) {
        self.style_value = "color:red".into();
        self.display_style = "display:flex;color:red".into();
    }

    pub fn toggle_visible(&mut self) {
        self.visible = !self.visible;
    }

    pub fn update_styles(&mut self) {
        self.alternate = !self.alternate;
        if self.alternate {
            self.style_value = "color:blue".into();
            self.display_style = "display:grid;color:blue".into();
        } else {
            self.style_value = "color:red".into();
            self.display_style = "display:flex;color:red".into();
        }
    }
}

/// Child with a parent-writable prop used by the child-host
/// directive plan regression.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanHostDirectiveChild.html")]
struct PlanHostDirectiveChild {
    #[prop]
    label: String,
}

#[handlers]
impl PlanHostDirectiveChild {}

/// Host whose lifted pp-if body contains a custom tag with
/// parent-scope host directives. Without StaticChildMount host
/// directive descriptors this shape required mount fallback to
/// install `:label` and `@click` on the custom tag.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfHostDirectiveHost.html")]
struct PlanIfHostDirectiveHost {
    open: bool,
    title: String,
    count: u32,
}

#[handlers]
impl PlanIfHostDirectiveHost {
    pub fn on_setup(&mut self) {
        self.title = "initial-host-title".into();
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    pub fn bump(&mut self) {
        self.count += 1;
        self.title = format!("host-count-{}", self.count);
    }
}

/// Child with a generated model channel used by the compiled
/// child-host `pp-model` regression.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanModelDirectiveChild.html")]
struct PlanModelDirectiveChild {
    #[model]
    value: String,
}

#[handlers]
impl PlanModelDirectiveChild {
    pub fn set_value(&mut self, value: String) {
        self.value = value;
    }
}

/// Host whose lifted pp-if body contains a custom tag with
/// parent-scope `pp-model:value`. This specifically covers the
/// `StaticChildHostModel` path, separate from `:prop` and
/// `@event`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfModelDirectiveHost.html")]
struct PlanIfModelDirectiveHost {
    open: bool,
    email: String,
}

#[handlers]
impl PlanIfModelDirectiveHost {
    pub fn on_setup(&mut self) {
        self.email = "initial-model-email".into();
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// Role component whose template root uses `pp-as`. The compiled
/// plan owns root-level attrs/listeners and applies them to the
/// hoisted user element.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanAsDirectiveChild.html", role = "interactive")]
struct PlanAsDirectiveChild {
    active: bool,
}

#[handlers]
impl PlanAsDirectiveChild {
    pub fn activate(&mut self) {
        self.active = true;
    }
}

/// Host whose lifted pp-if body contains a `pp-as` custom tag.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfAsDirectiveHost.html")]
struct PlanIfAsDirectiveHost {
    open: bool,
}

#[handlers]
impl PlanIfAsDirectiveHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// RFC-058 Phase 3.5f — child with `<slot>` (default) + two
/// named slots (`header`, `footer`). Drives the named-slot
/// fragment lifting evidence: each `<template pp-slot="NAME">`
/// in the parent's slot content lifts into its own fragment fn
/// and routes through `materialize_slot` by name.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanNamedSlotChild.html")]
struct PlanNamedSlotChild {}

#[handlers]
impl PlanNamedSlotChild {}

/// RFC-058 Phase 3.5f — host that fills two named slots
/// (`header`, `footer`) plus default content under a
/// `<plan-named-slot-child>`. The macro must emit three slot
/// fragments (`default`, `header`, `footer`) in the child-mount
/// entry; the runtime resolves each by name in
/// `materialize_slot` without falling back to the mount.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanNamedSlotHost.html")]
struct PlanNamedSlotHost {}

#[handlers]
impl PlanNamedSlotHost {}

/// RFC-058 Phase 3.5g — child whose `<slot>` declares
/// scoped-slot `:prop` bindings. The child's `current` field
/// drives those bindings; the host below uses `pp-let="ctx"`
/// to read them as `ctx.label`.
#[derive(Clone, Default, Serialize, Deserialize)]
struct PsscRow {
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanScopedSlotChild.html")]
struct PlanScopedSlotChild {
    current: PsscRow,
}

#[handlers]
impl PlanScopedSlotChild {}

/// RFC-058 Phase 3.5g — host that fills the child's scoped
/// `row` slot via `<template pp-slot="row" pp-let="ctx">`.
/// The macro must lift this into a slot fragment with
/// `scoped_let = Some("ctx")`; the runtime materialiser
/// constructs a `SlotScope` from the child's `:prop` bindings
/// and invokes the fragment against that scope so `ctx.label`
/// resolves through SlotScope's RFC-011 routing.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanScopedSlotHost.html")]
struct PlanScopedSlotHost {
    mark: String,
}

#[handlers]
impl PlanScopedSlotHost {
    pub fn on_setup(&mut self) {
        self.mark = "initial".into();
    }
    pub fn bump(&mut self) {
        self.mark = "bumped-from-scoped-slot".into();
    }
}

/// RFC-058 Phase 4.1 — host with one `<template pp-if>` site.
/// The classifier lifts the directive into a `StaticIfPlan`,
/// strips `pp-if` from the cleaned HTML, and the runtime
/// applier installs the controller via `if_::install`. The
/// `<template>` body stays on the mount's clone+walk path
/// (Phase 4.1d will lift it).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfHost.html")]
struct PlanIfHost {
    open: bool,
}

#[handlers]
impl PlanIfHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// RFC-058 Phase 4.3 — host with one `<template pp-teleport>`
/// site. The classifier lifts the directive into a
/// `StaticTeleportPlan`, strips `pp-teleport` from the cleaned
/// HTML, and the runtime applier resolves the target via
/// `teleport::install`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanTeleportHost.html")]
struct PlanTeleportHost {}

#[handlers]
impl PlanTeleportHost {}

/// RFC-058 Phase 6.2 — host with `{{expr}}` text interpolation
/// in two siblings (one mixed with static text, one bare) and
/// a third sibling with no interp. The macro lifts each
/// interpolated text node into a `StaticInterp` entry; the
/// applier installs effects per dynamic segment.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanInterpHost.html")]
struct PlanInterpHost {
    label: String,
    count: u32,
}

#[handlers]
impl PlanInterpHost {
    pub fn on_setup(&mut self) {
        self.label = "world".into();
        self.count = 3;
    }
    pub fn bump(&mut self) {
        self.count += 1;
    }
}

/// RFC-058 Phase 6.2 regression — host whose single carrier
/// element holds two interpolated text nodes separated by an
/// element child (`a {{x}}<em>middle</em>b {{y}}`). The macro
/// emits two `StaticInterp` entries both targeting the same
/// parent, with `text_index` 0 and 1 keyed against the original
/// DOM. compiled interp install must keep those indices valid even
/// though installing index 0 mutates the live text-node list
/// (inserts new siblings, removes the placeholder).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanInterpMultiHost.html")]
struct PlanInterpMultiHost {
    x: String,
    y: String,
}

#[handlers]
impl PlanInterpMultiHost {
    pub fn on_setup(&mut self) {
        self.x = "X".into();
        self.y = "Y".into();
    }
}

/// RFC-058 Phase 6.5 — fixture for the native `pp-model`
/// compiled-mount branch. The macro lifts `pp-model` into a
/// static native model entry, so no fallback mount pass is needed.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "StartCompiledModelHost.html")]
struct StartCompiledModelHost {
    value: String,
}

#[handlers]
impl StartCompiledModelHost {
    pub fn on_setup(&mut self) {
        self.value = "seed".into();
    }
}

/// RFC-058 Phase 3 hardening — host with `pp-ref` on a
/// custom child host. Drives the regression that without
/// classifier coverage for `pp-ref` on a non-HTML5 tag the
/// attr would fall to `Preserved` and flip
/// `requires_walker = true` (the cause of the date/time
/// picker fallbacks pre-fix).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanChildHostRefHost.html")]
struct PlanChildHostRefHost {}

#[handlers]
impl PlanChildHostRefHost {}

/// RFC-058 Phase 3 hardening — host whose root carries the
/// runtime-only `pp-roving.both` directive. The macro lifts it
/// into a `StaticOpaqueDirective` entry instead of preserving it
/// on the cleaned HTML and flipping `requires_walker`. The
/// applier dispatches it through the same registry the mount
/// uses, after every other plan entry has resolved (so the
/// container's items are in the DOM when roving installs).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanOpaqueDirectiveHost.html")]
struct PlanOpaqueDirectiveHost {}

#[handlers]
impl PlanOpaqueDirectiveHost {}

/// RFC-058 Phase 4.2c — pp-for row body lifting. Unkeyed list
/// with one `pp-text` binding per row. The macro emits a body
/// fragment fn that installs `pp-text` against the row's
/// `LoopScope` per iteration — no recursive walk on row clones.
#[derive(Clone, Default, Serialize, Deserialize)]
struct PfbhRow {
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanForBodyHost.html")]
struct PlanForBodyHost {
    rows: Vec<PfbhRow>,
}

#[handlers]
impl PlanForBodyHost {
    pub fn on_setup(&mut self) {
        self.rows = vec![
            PfbhRow {
                label: "alpha".into(),
            },
            PfbhRow {
                label: "beta".into(),
            },
        ];
    }
    pub fn add(&mut self) {
        let n = self.rows.len() + 1;
        self.rows.push(PfbhRow {
            label: format!("row-{n}"),
        });
    }
}

/// RFC-068 — SVG `<template pp-for>` is a controller anchor,
/// not an `HTMLTemplateElement`. The row body must materialise
/// through the SVG namespace and still receive compiled
/// bindings against the row scope.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Rfc068SvgLine {
    id: u32,
    x: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rfc068-svg-for-host",
    template_inline = r#"<svg class="r68-svg" viewBox="0 0 100 100">
        <g class="r68-lines">
          <template pp-for="line in lines" pp-key="line.id">
            <g class="r68-row" :data-line-id="line.id">
              <line class="r68-line"
                    :x1="line.x"
                    y1="0"
                    :x2="line.x"
                    y2="100"></line>
              <text class="r68-label"
                    :x="line.x"
                    y="10"
                    pp-text="line.label"></text>
            </g>
          </template>
        </g>
    </svg>"#
)]
struct Rfc068SvgForHost {
    lines: Vec<Rfc068SvgLine>,
}

#[handlers]
impl Rfc068SvgForHost {
    pub fn on_setup(&mut self) {
        self.lines = vec![
            Rfc068SvgLine {
                id: 1,
                x: 10,
                label: "ten".into(),
            },
            Rfc068SvgLine {
                id: 2,
                x: 90,
                label: "ninety".into(),
            },
        ];
    }
}

/// RFC-064 Phase 4 — keyed compiled row fixture that exercises
/// the specialized single-remove and two-swap reconcile paths.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Rfc064KeyedRow {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rfc064-keyed-fast-host",
    template_inline = r#"<div class="r64k-root">
        <button class="r64k-append" pp-on:click="append_row">append</button>
        <button class="r64k-prepend" pp-on:click="prepend_row">prepend</button>
        <button class="r64k-swap" pp-on:click="swap_rows">swap</button>
        <button class="r64k-remove" pp-on:click="remove_second">remove</button>
        <ul>
          <template pp-for="row in rows" pp-key="row.id">
            <li class="r64k-row" pp-text="row.label"></li>
          </template>
        </ul>
    </div>"#
)]
struct Rfc064KeyedFastHost {
    rows: Vec<Rfc064KeyedRow>,
}

#[handlers]
impl Rfc064KeyedFastHost {
    pub fn on_setup(&mut self) {
        self.rows = vec![
            Rfc064KeyedRow {
                id: 1,
                label: "one".into(),
            },
            Rfc064KeyedRow {
                id: 2,
                label: "two".into(),
            },
            Rfc064KeyedRow {
                id: 3,
                label: "three".into(),
            },
        ];
    }

    pub fn swap_rows(&mut self) {
        self.rows.swap(0, 2);
        pocopine::swap_list_indices_inline("rows", 0, 2);
    }

    pub fn append_row(&mut self) {
        self.rows.push(Rfc064KeyedRow {
            id: 4,
            label: "four".into(),
        });
        pocopine::append_list_inline("rows", 3, &self.rows[3..]);
    }

    pub fn prepend_row(&mut self) {
        let new_rows = vec![Rfc064KeyedRow {
            id: 0,
            label: "zero".into(),
        }];
        self.rows.splice(0..0, new_rows.clone());
        pocopine::prepend_list_inline("rows", &new_rows);
    }

    pub fn remove_second(&mut self) {
        self.rows.remove(1);
        pocopine::remove_list_at_inline("rows", 1);
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rfc064-generic-prepend-host",
    template_inline = r#"<div class="r64g-root">
        <button class="r64g-prepend" pp-on:click="prepend_row">prepend</button>
        <ul>
          <template pp-for="row in rows" pp-key="row.id">
            <li class="r64g-row" :data-generic="row.label">
              <span class="r64g-index" pp-text="$index"></span>
              <span class="r64g-label" pp-text="row.label"></span>
            </li>
          </template>
        </ul>
    </div>"#
)]
struct Rfc064GenericPrependHost {
    rows: Vec<Rfc064KeyedRow>,
}

#[handlers]
impl Rfc064GenericPrependHost {
    pub fn on_setup(&mut self) {
        self.rows = vec![
            Rfc064KeyedRow {
                id: 1,
                label: "one".into(),
            },
            Rfc064KeyedRow {
                id: 2,
                label: "two".into(),
            },
        ];
    }

    pub fn prepend_row(&mut self) {
        let new_rows = vec![Rfc064KeyedRow {
            id: 0,
            label: "zero".into(),
        }];
        self.rows.splice(0..0, new_rows.clone());
        pocopine::prepend_list_inline("rows", &new_rows);
    }
}

/// RFC-058 Phase 4.1d — pp-if with a body that carries `pp-text`
/// + `@click` against the parent scope. The body subtree
/// qualifies for fragment lifting (HTML5-native, plan-eligible
/// directives only), so the macro emits a body fragment fn that
/// installs both directives via the Phase 1 helpers — no
/// recursive walk involvement on the cloned body.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfBodyHost.html")]
struct PlanIfBodyHost {
    open: bool,
    label: String,
    count: u32,
}

#[handlers]
impl PlanIfBodyHost {
    pub fn on_setup(&mut self) {
        self.label = "initial".into();
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    pub fn bump(&mut self) {
        self.count += 1;
        self.label = format!("count-{}", self.count);
    }
}

/// RFC 062 — component mounts use the macro-emitted body as the
/// normal path.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-specialized",
    template_inline = r#"<div><span class="psa-value" pp-text="message"></span></div>"#
)]
struct PlanSpecialized {
    message: String,
}

#[handlers]
impl PlanSpecialized {
    pub fn on_setup(&mut self) {
        self.message = "specialized".into();
    }
}

/// RFC 062 — larger templates still use the same generated mount
/// path. There is no author-facing static-plan fallback knob.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-specialized-big",
    template_inline = r#"<div>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span pp-text="message"></span><span pp-text="message"></span>
        <span class="psb-last" pp-text="message"></span>
    </div>"#
)]
struct PlanSpecializedBig {
    message: String,
}

#[handlers]
impl PlanSpecializedBig {
    pub fn on_setup(&mut self) {
        self.message = "big-specialized".into();
    }
}

// ─── helpers ─────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

/// RFC-094 — three-branch conditional chain fixture.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-cond-chain-host",
    template_inline = r#"
<div class="pcc-root">
  <button class="pcc-bump" pp-on:click="bump">+</button>
  <template pp-if="count > 5">
    <p class="pcc-branch pcc-big">big</p>
  </template>
  <!-- comments between members are tolerated -->
  <template pp-else-if="count > 0">
    <p class="pcc-branch pcc-small">small</p>
  </template>
  <template pp-else>
    <p class="pcc-branch pcc-zero">zero</p>
  </template>
</div>"#
)]
struct PlanCondChainHost {
    count: i32,
}

#[handlers]
impl PlanCondChainHost {
    pub fn bump(&mut self) {
        self.count += 1;
    }
}

/// RFC-094 Phase 3 — externally-tagged enum driven by `pp-match`.
#[derive(Default, Serialize, Deserialize)]
enum MatchStatus {
    #[default]
    Idle,
    Loading,
    Ready(String),
    Err {
        code: i32,
    },
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-match-host",
    template_inline = r#"
<div class="pm-root">
  <button class="pm-next" pp-on:click="next">next</button>
  <template pp-match="status">
    <template pp-case="Idle | Loading">
      <p class="pm-arm pm-pending">pending</p>
    </template>
    <!-- comments between arms are tolerated -->
    <template pp-case="Ready" pp-let="msg">
      <p class="pm-arm pm-ready">{{msg}}</p>
    </template>
    <template pp-case="_">
      <p class="pm-arm pm-other">other</p>
    </template>
  </template>
</div>"#
)]
struct PlanMatchHost {
    status: MatchStatus,
}

#[handlers]
impl PlanMatchHost {
    pub fn next(&mut self) {
        self.status = match &self.status {
            MatchStatus::Idle => MatchStatus::Loading,
            MatchStatus::Loading => MatchStatus::Ready("one".into()),
            MatchStatus::Ready(s) if s == "one" => MatchStatus::Ready("two".into()),
            MatchStatus::Ready(_) => MatchStatus::Err { code: 7 },
            MatchStatus::Err { .. } => MatchStatus::Idle,
        };
    }
}

fn register_all() {
    PlanMatchHost::register();
    PlanCondChainHost::register();
    PlanTextEcho::register();
    Rfc064ExprHost::register();
    PlanForLoop::register();
    PlanChildLeaf::register();
    PlanChildHost::register();
    PlanSlotChild::register();
    PlanSlotHostStatic::register();
    PlanIfHost::register();
    PlanForMixed::register();
    PlanTeleportHost::register();
    PlanIfBodyHost::register();
    PlanForBodyHost::register();
    Rfc068SvgForHost::register();
    Rfc064KeyedFastHost::register();
    Rfc064GenericPrependHost::register();
    PlanSlotDynamicHost::register();
    PlanSlotInterpHost::register();
    PlanCompoundHost::register();
    PlanDeferredSlotChild::register();
    PlanCompoundDeferredHost::register();
    PlanTeleportSlotChild::register();
    PlanCompoundTeleportHost::register();
    PlanRowPortalHost::register();
    PlanLifecycleLeaf::register();
    PlanIfChildHost::register();
    PlanComponentDirectiveChild::register();
    PlanComponentDirectiveHost::register();
    PlanShowStyleHost::register();
    PlanHostDirectiveChild::register();
    PlanIfHostDirectiveHost::register();
    PlanModelDirectiveChild::register();
    PlanIfModelDirectiveHost::register();
    PlanAsDirectiveChild::register();
    PlanIfAsDirectiveHost::register();
    PlanNamedSlotChild::register();
    PlanNamedSlotHost::register();
    PlanScopedSlotChild::register();
    PlanScopedSlotHost::register();
    PlanOpaqueDirectiveHost::register();
    PlanChildHostRefHost::register();
    PlanInterpHost::register();
    PlanInterpMultiHost::register();
    StartCompiledModelHost::register();
    PlanSpecialized::register();
    PlanSpecializedBig::register();
}

fn mount(host_html: &str) -> Element {
    register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    mount_registered_tags(&host);
    host
}

/// RFC 061 Phase 3 — test-only compiled root discovery.
/// QuerySelectorAll the union selector of every registered tag,
/// mount each via the typed
/// `mount_child_component` path, then run
/// `finalize_compiled_subtree` to fire mount/ready hooks.
fn mount_registered_tags(host: &Element) {
    let names = pocopine_core::templates::registered_template_names();
    if !names.is_empty() {
        let selector = names.join(",");
        if let Ok(matches) = host.query_selector_all(&selector) {
            for i in 0..matches.length() {
                let Some(node) = matches.item(i) else {
                    continue;
                };
                let Ok(el) = node.dyn_into::<Element>() else {
                    continue;
                };
                if !host.contains(Some(el.as_ref())) {
                    continue;
                }
                let tag = el.local_name();
                pocopine_core::mount::mount_child_component(&el, &tag);
                pocopine_core::mount::finalize_compiled_subtree(&el);
            }
        }
    }
    if let Ok(outlets) = host.query_selector_all("pp-outlet") {
        for i in 0..outlets.length() {
            let Some(node) = outlets.item(i) else {
                continue;
            };
            if let Ok(el) = node.dyn_into::<Element>() {
                pocopine_core::router::set_outlet(el);
            }
        }
    }
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn read(host: &Element, sel: &str) -> String {
    let el = host
        .query_selector(sel)
        .unwrap()
        .unwrap_or_else(|| panic!("missing selector {sel}"));
    el.dyn_into::<HtmlElement>()
        .unwrap()
        .inner_text()
        .trim()
        .to_string()
}

// ─── tests ───────────────────────────────────────────────────────

/// A plan-eligible template registers a `StaticTemplatePlan`
/// against its component tag, and mounting / unmounting the
/// component does not increment the fail-fast counter (every
/// `node_path` resolves and every `expr_src` parses).
#[wasm_bindgen_test]
async fn plan_eligible_template_registers_and_does_not_fail() {
    register_all();
    reset_plan_failure_count();

    assert!(
        template_plan_for("plan-text-echo").is_some(),
        "macro should emit a template plan for `<plan-text-echo>` (one `pp-text` on a native tag)",
    );

    let host = mount("<plan-text-echo></plan-text-echo>");
    tick().await;

    assert_eq!(
        plan_failure_count(),
        0,
        "no plan-install failure should fire for a clean fixture mount",
    );

    host.remove();
    tick().await;

    assert_eq!(
        plan_failure_count(),
        0,
        "no plan-install failure should fire across the unmount cycle",
    );
}

#[wasm_bindgen_test]
async fn rfc062_generated_mount_handles_small_and_large_components() {
    let small = mount("<plan-specialized></plan-specialized>");
    tick().await;
    assert_eq!(read(&small, ".psa-value"), "specialized");
    small.remove();

    let big = mount("<plan-specialized-big></plan-specialized-big>");
    tick().await;
    assert_eq!(read(&big, ".psb-last"), "big-specialized");
    big.remove();
}

#[wasm_bindgen_test]
async fn rfc062_generated_mount_covers_slot_interp_opaque_and_native_model_entries() {
    reset_plan_failure_count();
    let fixtures = [
        "<plan-slot-host-static></plan-slot-host-static>",
        "<plan-interp-host></plan-interp-host>",
        "<plan-opaque-directive-host></plan-opaque-directive-host>",
        "<start-compiled-model-host></start-compiled-model-host>",
    ];

    for html in fixtures {
        let host = mount(html);
        tick().await;
        assert_eq!(plan_failure_count(), 0, "{html} should mount cleanly");
        host.remove();
    }
}

#[wasm_bindgen_test]
async fn rfc064_compiles_safe_expression_envelope_and_preserves_fallback() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("rfc064-expr-host").expect("rfc064 expression fixture has a plan");
    assert_eq!(plan.bindings.len(), 7);

    let compiled_by_expr = |expr: &str| {
        plan.bindings
            .iter()
            .find(|binding| binding.expr_src == expr)
            .map(|binding| binding.compiled.is_some())
            .unwrap_or(false)
    };
    for expr in [
        "label",
        "nested.label",
        "'literal'",
        "!hidden",
        "count > 2",
        "ready && count > 2",
    ] {
        assert!(compiled_by_expr(expr), "{expr} should use StaticExpr");
    }
    assert!(
        !compiled_by_expr("count > 2 ? 'yes' : 'no'"),
        "ternary should stay on the runtime evaluator fallback",
    );

    let host = mount("<rfc064-expr-host></rfc064-expr-host>");
    tick().await;

    assert_eq!(read(&host, ".r64-ident"), "identifier");
    assert_eq!(read(&host, ".r64-field"), "field");
    assert_eq!(read(&host, ".r64-literal"), "literal");
    assert_eq!(read(&host, ".r64-not"), "true");
    assert_eq!(read(&host, ".r64-compare"), "true");
    assert_eq!(read(&host, ".r64-bool"), "true");
    assert_eq!(read(&host, ".r64-fallback"), "yes");
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// A planned `pp-text` whose bound value contains literal
/// `{ ... }` braces must render the braces as-is — the plan
/// owns the element's text and nothing may re-tokenise
/// `value: {count} ok` on a reactive update.
#[wasm_bindgen_test]
async fn planned_pp_text_does_not_reinterpolate_brace_payload() {
    let host = mount("<plan-text-echo></plan-text-echo>");
    tick().await;

    assert_eq!(read(&host, ".ple-msg"), "value: {count} ok");

    host.remove();
}

/// RFC-058 Phase 4.2 — pp-for itself graduates into a
/// `StaticForPlan` entry on the template plan. This template
/// has no other plan-relevant content; the plan registers
/// solely for the pp-for site and the runtime applier hands
/// the parsed `<item> in <items>` to `for_::install`. Row
/// content still flows through the RFC-054 row-plan registry
/// via the §6.2-layered `data-pp-row-plan` stamp.
#[wasm_bindgen_test]
async fn template_with_only_pp_for_emits_for_plan() {
    register_all();
    let plan = template_plan_for("plan-for-loop")
        .expect("pp-for itself counts as a plan entry post-Phase-4.2");
    assert_eq!(plan.for_plans.len(), 1, "exactly one pp-for site");
    assert_eq!(plan.for_plans[0].item_name, "row");
    assert_eq!(plan.for_plans[0].items_expr, "rows");
    assert_eq!(plan.for_plans[0].key_expr, Some("row.id"));

    let host = mount("<plan-for-loop></plan-for-loop>");
    tick().await;

    let li = host
        .query_selector("li")
        .unwrap()
        .expect("pp-for row must mount via the for-plan + row-plan path");
    assert_eq!(li.text_content().as_deref(), Some("alpha"));

    host.remove();
}

/// RFC-058 Phase 4.2c — pp-for row body lifting. Unkeyed
/// list with no row plan; the macro emits a body fragment fn
/// the runtime invokes per row instead of the legacy
/// clone-and-walk path. The fragment installs `pp-text` against
/// the row's `LoopScope` per iteration; appending a new row
/// drives the effect to mount + bind a fresh `<li>` without
/// the mount touching the row.
#[wasm_bindgen_test]
async fn macro_emitted_pp_for_row_body_fragment_installs_per_row() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-for-body-host")
        .expect("plan-for-body-host registers a template plan");
    assert_eq!(plan.for_plans.len(), 1);
    assert!(
        plan.for_plans[0].body.is_some(),
        "unkeyed pp-for body with one pp-text must lift",
    );

    let host = mount("<plan-for-body-host></plan-for-body-host>");
    tick().await;

    let initial_rows = host.query_selector_all(".pfbh-row").unwrap();
    assert_eq!(initial_rows.length(), 2);
    assert_eq!(
        initial_rows.get(0).unwrap().text_content().as_deref(),
        Some("alpha"),
    );
    assert_eq!(
        initial_rows.get(1).unwrap().text_content().as_deref(),
        Some("beta"),
    );

    // Append a row → effect re-runs → new row mounts via
    // body fragment.
    let add = host.query_selector(".pfbh-add").unwrap().unwrap();
    add.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let after = host.query_selector_all(".pfbh-row").unwrap();
    assert_eq!(after.length(), 3);
    assert_eq!(
        after.get(2).unwrap().text_content().as_deref(),
        Some("row-3"),
    );

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

#[wasm_bindgen_test]
async fn svg_pp_for_mounts_rows_in_svg_namespace() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("rfc068-svg-for-host")
        .expect("rfc068-svg-for-host registers a template plan");
    assert_eq!(plan.for_plans.len(), 1);
    assert!(
        plan.for_plans[0].body.is_some(),
        "SVG pp-for rows with SVG attribute bindings must lift into a body fragment",
    );

    let host = mount("<rfc068-svg-for-host></rfc068-svg-for-host>");
    tick().await;

    let rows = host.query_selector_all(".r68-row").unwrap();
    assert_eq!(rows.length(), 2, "only mounted rows should be visible");

    // RFC-094 Phase 4 — the SVG pseudo-template (a foreign
    // element, not an HTMLTemplateElement) swaps for the comment
    // anchor like every pp-for site; comments are valid SVG nodes.
    assert!(
        host.query_selector("svg template").unwrap().is_none(),
        "the SVG controller anchor must leave the live DOM",
    );

    let line = host
        .query_selector(".r68-line")
        .unwrap()
        .expect("first SVG line mounted")
        .dyn_into::<Element>()
        .unwrap();
    assert_eq!(
        line.namespace_uri().as_deref(),
        Some("http://www.w3.org/2000/svg"),
    );
    assert_eq!(line.get_attribute("x1").as_deref(), Some("10"));
    assert_eq!(line.get_attribute("x2").as_deref(), Some("10"));

    let label = host
        .query_selector(".r68-label")
        .unwrap()
        .expect("first SVG text mounted")
        .dyn_into::<Element>()
        .unwrap();
    assert_eq!(
        label.namespace_uri().as_deref(),
        Some("http://www.w3.org/2000/svg"),
    );
    assert_eq!(label.get_attribute("x").as_deref(), Some("10"));
    assert_eq!(label.text_content().as_deref(), Some("ten"));
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

#[wasm_bindgen_test]
async fn rfc064_keyed_remove_and_swap_reuse_dom_nodes() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;

    let initial = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(initial.length(), 3);
    let one = initial.item(0).unwrap();
    let two = initial.item(1).unwrap();
    let three = initial.item(2).unwrap();

    let swap = host.query_selector(".r64k-swap").unwrap().unwrap();
    swap.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let swapped = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(swapped.length(), 3);
    assert!(swapped.item(0).unwrap().is_same_node(Some(&three)));
    assert!(swapped.item(1).unwrap().is_same_node(Some(&two)));
    assert!(swapped.item(2).unwrap().is_same_node(Some(&one)));
    assert_eq!(
        swapped.item(0).unwrap().text_content().as_deref(),
        Some("three"),
    );

    let remove = host.query_selector(".r64k-remove").unwrap().unwrap();
    remove.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let removed = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(removed.length(), 2);
    assert!(removed.item(0).unwrap().is_same_node(Some(&three)));
    assert!(removed.item(1).unwrap().is_same_node(Some(&one)));
    assert_eq!(
        removed.item(0).unwrap().text_content().as_deref(),
        Some("three"),
    );
    assert_eq!(
        removed.item(1).unwrap().text_content().as_deref(),
        Some("one"),
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

/// RFC-095 W4 — differential gate: the mutation channel's cold
/// mount + append must produce byte-identical DOM to the direct
/// web-sys path, with the proxy elision intact in both modes.
#[wasm_bindgen_test]
async fn channel_and_direct_keyed_mounts_match() {
    register_all();
    reset_plan_failure_count();

    let render_pass = |label: &str, enabled: bool| {
        pocopine_core::mutation_channel::set_enabled(enabled);
        let _ = label;
    };

    // Direct pass — capture html AND node-identity behavior so
    // the direct lane keeps real coverage while the channel is
    // the default for every other keyed test in this module.
    render_pass("direct", false);
    let mints_before = pocopine_core::scope::proxies_minted_count();
    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;
    let first_before = host.query_selector(".r64k-row").unwrap().unwrap();
    let append = host.query_selector(".r64k-append").unwrap().unwrap();
    append.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert!(
        host.query_selector(".r64k-row")
            .unwrap()
            .unwrap()
            .is_same_node(Some(&first_before)),
        "direct append must reuse existing row nodes",
    );
    let direct_html = host
        .query_selector("rfc064-keyed-fast-host")
        .unwrap()
        .unwrap()
        .inner_html();
    let direct_elided = pocopine_core::scope::proxies_minted_count() == mints_before;
    host.remove();
    tick().await;
    // Restore the default BEFORE asserting — a failure here must
    // not leak channel-off into every later test in the session.
    pocopine_core::mutation_channel::set_enabled(true);
    assert!(direct_elided, "direct keyed mount stays proxy-elided");

    // Channel pass.
    render_pass("channel", true);
    let mints_before = pocopine_core::scope::proxies_minted_count();
    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;
    let append = host.query_selector(".r64k-append").unwrap().unwrap();
    append.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    let channel_html = host
        .query_selector("rfc064-keyed-fast-host")
        .unwrap()
        .unwrap()
        .inner_html();
    assert_eq!(
        pocopine_core::scope::proxies_minted_count(),
        mints_before,
        "channel keyed mount stays proxy-elided",
    );
    host.remove();
    tick().await;

    assert_eq!(
        direct_html, channel_html,
        "channel and direct mounts must produce identical DOM",
    );
    assert_eq!(plan_failure_count(), 0);
}

#[wasm_bindgen_test]
async fn rfc064_keyed_append_reuses_existing_dom_nodes() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;

    let initial = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(initial.length(), 3);
    let one = initial.item(0).unwrap();
    let two = initial.item(1).unwrap();
    let three = initial.item(2).unwrap();

    let append = host.query_selector(".r64k-append").unwrap().unwrap();
    append.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let rows = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(rows.length(), 4);
    assert!(rows.item(0).unwrap().is_same_node(Some(&one)));
    assert!(rows.item(1).unwrap().is_same_node(Some(&two)));
    assert!(rows.item(2).unwrap().is_same_node(Some(&three)));
    assert_eq!(
        rows.item(3).unwrap().text_content().as_deref(),
        Some("four")
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

#[wasm_bindgen_test]
async fn rfc064_keyed_prepend_reuses_existing_dom_nodes() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;

    let initial = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(initial.length(), 3);
    let one = initial.item(0).unwrap();
    let two = initial.item(1).unwrap();
    let three = initial.item(2).unwrap();

    let prepend = host.query_selector(".r64k-prepend").unwrap().unwrap();
    prepend.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let rows = host.query_selector_all(".r64k-row").unwrap();
    assert_eq!(rows.length(), 4);
    assert_eq!(
        rows.item(0).unwrap().text_content().as_deref(),
        Some("zero")
    );
    assert!(rows.item(1).unwrap().is_same_node(Some(&one)));
    assert!(rows.item(2).unwrap().is_same_node(Some(&two)));
    assert!(rows.item(3).unwrap().is_same_node(Some(&three)));

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

#[wasm_bindgen_test]
async fn rfc064_generic_prepend_refreshes_reused_row_indexes() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<rfc064-generic-prepend-host></rfc064-generic-prepend-host>");
    tick().await;

    let initial = host.query_selector_all(".r64g-row").unwrap();
    assert_eq!(initial.length(), 2);
    let one = initial.item(0).unwrap();
    let two = initial.item(1).unwrap();
    assert_eq!(read(&host, ".r64g-row:nth-child(1) .r64g-index"), "0");
    assert_eq!(read(&host, ".r64g-row:nth-child(2) .r64g-index"), "1");

    let prepend = host.query_selector(".r64g-prepend").unwrap().unwrap();
    prepend.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let rows = host.query_selector_all(".r64g-row").unwrap();
    assert_eq!(rows.length(), 3);
    assert_eq!(read(&host, ".r64g-row:nth-child(1) .r64g-index"), "0");
    assert_eq!(read(&host, ".r64g-row:nth-child(2) .r64g-index"), "1");
    assert_eq!(read(&host, ".r64g-row:nth-child(3) .r64g-index"), "2");
    assert_eq!(read(&host, ".r64g-row:nth-child(1) .r64g-label"), "zero");
    assert!(rows.item(1).unwrap().is_same_node(Some(&one)));
    assert!(rows.item(2).unwrap().is_same_node(Some(&two)));

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

/// RFC-058 Phase 3.5c — dynamic slot fragment lifting. The
/// macro emits a fragment fn whose body installs `pp-text` and
/// `@click` against the parent scope via `stamp_dynamic_slot`
/// and compiled fragment install. The slot content reads parent state
/// (`title`) and writes to it (`bump`) — proving the
/// parent_proxy thread captures the right scope at install
/// time and the bindings/listeners install correctly inside
/// the slotted subtree.
#[wasm_bindgen_test]
async fn macro_emitted_dynamic_slot_fragment_installs_against_parent() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-slot-dynamic-host")
        .expect("plan-slot-dynamic-host has plan-eligible directives");
    assert_eq!(plan.child_mounts.len(), 1, "one child-mount site");
    assert_eq!(
        plan.child_mounts[0].slots.len(),
        1,
        "default slot lifts into one fragment",
    );
    assert_eq!(plan.child_mounts[0].slots[0].name, "default");

    let host = mount("<plan-slot-dynamic-host></plan-slot-dynamic-host>");
    tick().await;

    let label = host
        .query_selector(".psdh-label")
        .unwrap()
        .expect("slot label must mount via the dynamic fragment");
    assert_eq!(
        label.text_content().as_deref(),
        Some("initial"),
        "pp-text in slot must read parent scope's `title`",
    );

    let bump = host.query_selector(".psdh-bump").unwrap().unwrap();
    bump.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        label.text_content().as_deref(),
        Some("count-1"),
        "@click in slot must dispatch to parent scope's `bump`",
    );

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// Bare `{{ }}` interpolation as slot content compiles in the
/// author's scope — parity with `pp-text` in the same position.
/// The top-level text node lifts into a dynamic slot fragment
/// (root-anchored interp entry) that renders and reactively
/// updates; no raw braces ever reach the DOM.
#[wasm_bindgen_test]
async fn slot_content_interpolation_renders_in_author_scope() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<plan-slot-interp-host></plan-slot-interp-host>");
    tick().await;

    let shell = host
        .query_selector(".psc-shell")
        .unwrap()
        .expect("slot child shell must mount");
    let text = shell.text_content().unwrap_or_default();
    assert!(
        text.contains("first ready"),
        "slot interp must render the author scope's field: {text:?}"
    );
    assert!(
        !text.contains("{{"),
        "no raw braces may reach the DOM: {text:?}"
    );

    host.query_selector(".psih-bump")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    let text = shell.text_content().unwrap_or_default();
    assert!(
        text.contains("second ready"),
        "slot interp must update reactively: {text:?}"
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

/// Compound-component slot projection (the AkContextMenu shape): a `<slot>`
/// outlet inside a CHILD component's slot content must still receive the
/// consumer's projected content — traversing the child boundary — and no
/// literal unresolved `<slot>` element may survive in the DOM.
#[wasm_bindgen_test]
async fn slot_outlet_inside_child_slot_content_projects_consumer_content() {
    register_all();
    reset_plan_failure_count();

    let host =
        mount("<plan-compound-host><span class=\"pch-item\">projected</span></plan-compound-host>");
    tick().await;

    // The consumer's content traversed plan-compound-host's template INTO
    // plan-slot-child's shell.
    let item = host
        .query_selector(".psc-shell .pch-item")
        .unwrap()
        .expect("consumer content must project through the child boundary");
    assert_eq!(item.text_content().as_deref(), Some("projected"));

    // The sibling bound child compiled in the author's scope (symptom 3:
    // one rejected <slot> used to leave `:label` a raw attribute).
    let badge = host
        .query_selector(".psc-shell .phdc-label")
        .unwrap()
        .expect("bound child inside the slot content must mount");
    assert_eq!(badge.text_content().as_deref(), Some("badge-ok"));

    // No inert outlet left behind.
    assert!(
        host.query_selector("slot").unwrap().is_none(),
        "no literal <slot> may survive materialisation: {:?}",
        host.inner_html()
    );

    host.remove();
}

/// The deferred variant (the dropdown-portal shape): the compound host's
/// outlet rides the child's slot content into a `pp-if` body, so the
/// consumer's content must materialize LATE — when the branch opens — not
/// at mount. Before the fix the whole chain silently dropped the content
/// and cloned an inert literal `<slot>` on open.
#[wasm_bindgen_test]
async fn slot_outlet_in_deferred_child_body_projects_on_open() {
    register_all();
    reset_plan_failure_count();

    let host = mount(
        "<plan-compound-deferred-host><span class=\"pcdh-item\">late</span>\
         </plan-compound-deferred-host>",
    );
    tick().await;

    // Closed: the deferred body (and the projected content) is absent.
    assert!(
        host.query_selector(".pdsc-body").unwrap().is_none(),
        "deferred body must not exist before open"
    );

    host.query_selector(".pdsc-open")
        .unwrap()
        .expect("child shell mounts")
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    let item = host
        .query_selector(".pdsc-body .pcdh-item")
        .unwrap()
        .expect("consumer content must project into the opened deferred body");
    assert_eq!(item.text_content().as_deref(), Some("late"));
    assert!(
        host.query_selector("slot").unwrap().is_none(),
        "no literal <slot> may survive the deferred materialisation: {:?}",
        host.inner_html()
    );

    host.remove();
}

/// The full dropdown-portal shape (`pp-if` + `pp-teleport="body"`): the
/// projected consumer content must materialize on open AND land in the
/// teleport target (document body), outside the host tree.
#[wasm_bindgen_test]
async fn slot_outlet_in_teleported_child_body_projects_on_open() {
    register_all();
    reset_plan_failure_count();

    let host = mount(
        "<plan-compound-teleport-host><span class=\"pcth-item\">ported</span>\
         </plan-compound-teleport-host>",
    );
    tick().await;

    let body = doc().body().unwrap();
    assert!(
        body.query_selector(".ptsc-body").unwrap().is_none(),
        "teleported body must not exist before open"
    );

    host.query_selector(".ptsc-open")
        .unwrap()
        .expect("child shell mounts")
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    // The content teleported to <body> WITH the projected consumer item.
    let ported = body
        .query_selector(".ptsc-body .pcth-item")
        .unwrap()
        .expect("consumer content must project into the teleported body");
    assert_eq!(ported.text_content().as_deref(), Some("ported"));
    assert!(
        body.query_selector(".ptsc-body slot").unwrap().is_none(),
        "no literal <slot> may survive in the teleported body"
    );

    // Cleanup: drop the teleported node too, so later tests see a clean body.
    if let Some(node) = body.query_selector(".ptsc-body").unwrap() {
        node.remove();
    }
    host.remove();
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = window() {
            let _ = w
                .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The exact AgenKitty-11 timeline: the panel's close (leave) animation is
/// STILL RUNNING when the row unmounts. The row teardown must reap the
/// mid-leave clone immediately — not wait for a completion that may never
/// come — and the leave's late backstop callback must then no-op instead of
/// double-freeing.
#[wasm_bindgen_test]
async fn deleting_a_row_mid_close_animation_reaps_the_portal_clone() {
    register_all();
    reset_plan_failure_count();

    // Give the clone a real CSS transition (the leave path discovers
    // animated elements via the fixture's `pp-transition:leave*` attrs;
    // the stylesheet makes the leave classes actually animate).
    let style = doc().create_element("style").unwrap();
    style.set_text_content(Some(
        ".ptsc-body { opacity: 1; transition: opacity 0.25s linear; } \
         .ptsc-leave-to { opacity: 0; }",
    ));
    doc().head().unwrap().append_child(&style).unwrap();

    let host = mount("<plan-row-portal-host></plan-row-portal-host>");
    tick().await;
    let body = doc().body().unwrap();

    // Open, then start CLOSING (leave animation begins; clone still in DOM).
    host.query_selector(".prph-row .ptsc-open")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    host.query_selector(".prph-row .ptsc-close")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    // Self-check that the test exercises what it claims: the leave window
    // must actually be OPEN here — the clone still in the DOM, mid-leave.
    // Without this the test would pass vacuously (a synchronous close
    // removes the clone before the row delete ever runs).
    assert!(
        body.query_selector(".ptsc-body").unwrap().is_some(),
        "close must start an ANIMATED leave — clone gone means the leave \
         window never opened and this test is not testing mid-leave"
    );

    // Delete the row inside the leave window.
    host.query_selector(".prph-remove")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    assert!(
        body.query_selector(".ptsc-body").unwrap().is_none(),
        "row teardown must reap the mid-leave clone immediately"
    );

    // Ride out the transition + the 1s leave backstop: the late completion
    // must be a no-op (no resurrection, no double-free panic).
    sleep_ms(1200).await;
    assert!(
        body.query_selector(".ptsc-body").unwrap().is_none(),
        "late leave completion must not resurrect the clone"
    );

    style.remove();
    host.remove();
}

/// The compiled single-remove FAST path must release the removed row's loop
/// scope + reactive bookkeeping too — it early-returns before the drain path,
/// so it needs its own post-removal release (review finding on the
/// drain-path fix).
#[wasm_bindgen_test]
async fn compiled_single_remove_fast_path_releases_the_row_scope() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;
    assert_eq!(host.query_selector_all(".r64k-row").unwrap().length(), 3);

    // Compiled rows have no per-row effects (one list-level watcher drives
    // them), so the leak is only visible in the SCOPE registry: removing a
    // row must evict exactly its LoopScope.
    let before = pocopine_core::scope::Scope::count();
    host.query_selector(".r64k-remove")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert_eq!(host.query_selector_all(".r64k-row").unwrap().length(), 2);
    let after = pocopine_core::scope::Scope::count();

    assert_eq!(
        after,
        before - 1,
        "fast-path removal must evict the removed row's loop scope \
         (scopes {before} -> {after})",
    );

    host.remove();
}

/// Row removal must be effect-neutral: everything nested in the row —
/// component scopes, reactive effects, listeners — releases with it. Before
/// the fix the drain paths only detached the DOM (relying on a
/// MutationObserver removed in RFC-058 Phase 6.5), leaking all of it.
#[wasm_bindgen_test]
async fn removing_a_keyed_row_releases_its_nested_effects() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<plan-row-portal-host></plan-row-portal-host>");
    tick().await;

    async fn remove_once(host: &Element) {
        host.query_selector(".prph-remove")
            .unwrap()
            .unwrap()
            .dyn_ref::<HtmlElement>()
            .unwrap()
            .click();
        tick().await;
    }

    // Warm-up removal absorbs one-time lazy state, then a steady-state
    // removal must strictly DROP live effects (the removed row's).
    remove_once(&host).await; // 2 rows → 1
    let (before, _) = pocopine_core::reactive::stats();
    remove_once(&host).await; // 1 row → 0
    let (after, _) = pocopine_core::reactive::stats();
    assert!(
        after < before,
        "removing a row must release its nested effects (before={before}, after={after})"
    );

    host.remove();
}

/// Deleting a keyed pp-for row must reap any portal clone its subtree
/// teleported to `<body>` (AgenKitty finding 11: deleting a tree row while
/// its context menu was open/closing orphaned the panel at the viewport
/// corner indefinitely). Row teardown is table-driven — it never walks the
/// row's DOM — so the clone stashed on the portal's origin element inside
/// the row was never released.
#[wasm_bindgen_test]
async fn deleting_a_row_reaps_its_teleported_portal_clone() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<plan-row-portal-host></plan-row-portal-host>");
    tick().await;
    let body = doc().body().unwrap();

    // Open the FIRST row's portal → clone lands at <body>.
    host.query_selector(".prph-row .ptsc-open")
        .unwrap()
        .expect("row's portal trigger mounts")
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert!(
        body.query_selector(".ptsc-body .prph-content")
            .unwrap()
            .is_some(),
        "portal clone must be at <body> while the row lives"
    );

    // Delete the first row while its portal is OPEN.
    host.query_selector(".prph-remove")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    assert!(
        host.query_selector_all(".prph-row").unwrap().length() == 1,
        "one row must remain"
    );
    // The orphan: the clone must be gone with its row.
    assert!(
        body.query_selector(".ptsc-body").unwrap().is_none(),
        "deleting the row must reap its teleported clone: {:?}",
        body.query_selector(".ptsc-body")
            .unwrap()
            .map(|n| n.outer_html())
    );

    // Cleanup for later tests.
    if let Some(node) = body.query_selector(".ptsc-body").unwrap() {
        node.remove();
    }
    host.remove();
}

/// The bare-slot-interp effect must RELEASE on unmount. Its install
/// resolves against `stamp_dynamic_slot_with`'s detached wrapper, which
/// never enters the DOM — the runtime re-homes the effect onto the live
/// element receiving the spliced content, so subtree release disposes it
/// (previously it leaked, keeping the parent scope + text node alive
/// across remounts).
#[wasm_bindgen_test]
async fn slot_interp_effect_releases_on_unmount() {
    register_all();
    reset_plan_failure_count();

    async fn cycle() {
        let host = mount("<plan-slot-interp-host></plan-slot-interp-host>");
        tick().await;
        pocopine_core::mount::release_compiled_subtree(&host);
        host.remove();
        tick().await;
    }

    // The warm-up absorbs any one-time global effects; after it, a
    // steady-state mount/release cycle must be effect-neutral.
    cycle().await;
    let (baseline, _) = pocopine_core::reactive::stats();
    cycle().await;
    let (after, _) = pocopine_core::reactive::stats();
    assert_eq!(
        after, baseline,
        "a slot-interp mount/release cycle must not leak effects"
    );
}

/// RFC-058 Phase 4.1d — body fragment lifting. The macro emits
/// a body fragment fn that stamps the cleaned body HTML and
/// installs both `pp-text` and `@click` against the parent
/// scope via the Phase 1 helpers. Toggling pp-if in/out
/// exercises mount + unmount; clicking the body's @click
/// handler proves the listener installed against the parent
/// scope; updating the parent's `label` field through that
/// handler proves the pp-text effect is reactively wired.
#[wasm_bindgen_test]
async fn macro_emitted_pp_if_body_fragment_installs_directives() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-if-body-host")
        .expect("plan-if-body-host registers a template plan");
    assert_eq!(plan.if_plans.len(), 1);
    assert!(
        plan.if_plans[0].body.is_some(),
        "body with pp-text + @click on HTML5 natives must lift",
    );

    let host = mount("<plan-if-body-host></plan-if-body-host>");
    tick().await;

    // Toggle open → body mounts via fragment.
    let toggle = host.query_selector(".pibh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let label = host
        .query_selector(".pibh-label")
        .unwrap()
        .expect("body must mount via the fragment");
    assert_eq!(
        label.text_content().as_deref(),
        Some("initial"),
        "pp-text in body must read parent scope's `label`",
    );

    // Click the body's @click → parent's `bump` fires → `label`
    // updates → reactive effect re-runs → DOM changes.
    let bump = host.query_selector(".pibh-bump").unwrap().unwrap();
    bump.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        label.text_content().as_deref(),
        Some("count-1"),
        "@click in body must dispatch to parent scope's `bump`",
    );

    // Toggle off → body unmounts. Tracked effects on the body
    // root must release so a subsequent re-mount + label change
    // doesn't cause double-fires.
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert!(host.query_selector(".pibh-label").unwrap().is_none());

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 mount-removal slice — a lifted body whose root is a
/// child component should not need the recursive mount just to
/// finish post-order lifecycle. The plan mounts the child, then
/// `finalize_compiled_subtree` fires the child's `on_mount`
/// without scanning attributes.
#[wasm_bindgen_test]
async fn lifted_pp_if_child_mount_finalizes_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-if-child-host")
        .expect("plan-if-child-host registers a template plan");
    assert_eq!(plan.if_plans.len(), 1);
    assert!(
        plan.if_plans[0].body.is_some(),
        "pp-if body rooted at a planned child component must lift",
    );

    let host = mount("<plan-if-child-host></plan-if-child-host>");
    tick().await;

    let toggle = host.query_selector(".pich-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let leaf = host
        .query_selector(".pll-leaf")
        .unwrap()
        .expect("child component in lifted body should mount");
    assert_eq!(
        leaf.text_content().as_deref(),
        Some("mounted-without-walk"),
        "child on_mount should fire through compiled finalization",
    );
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// Component hosts use the compiled child-mount path, so directives authored
/// on the host must be carried by `StaticChildMount` instead of relying on a
/// recursive walker. This also pins a direct `pp-if` component root with
/// projected content, the shape used by button/row components in applications.
#[wasm_bindgen_test]
async fn component_hosts_support_pp_if_bodies_and_pp_show() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-component-directive-host")
        .expect("component-host directive fixture has a template plan");
    assert_eq!(plan.if_plans.len(), 1);
    assert!(
        plan.if_plans[0].body.is_some(),
        "pp-if body rooted at a slotted child component must lift",
    );

    let host = mount("<plan-component-directive-host></plan-component-directive-host>");
    tick().await;

    assert!(
        host.query_selector("plan-component-directive-child[data-pcdh='if']")
            .unwrap()
            .is_none()
    );

    let shown = host
        .query_selector("plan-component-directive-child[data-pcdh='show']")
        .unwrap()
        .expect("pp-show keeps the component mounted")
        .dyn_into::<HtmlElement>()
        .unwrap();
    assert_eq!(
        shown.style().get_property_value("display").unwrap(),
        "none",
        "false pp-show must override the component host's display: contents rule",
    );

    host.query_selector(".pcdh-toggle-if")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    let branch = host
        .query_selector("plan-component-directive-child[data-pcdh='if']")
        .unwrap()
        .expect("truthy pp-if must stamp its component-rooted body");
    assert!(
        branch.text_content().unwrap_or_default().contains("Delete"),
        "the component's projected default slot content must survive lifting",
    );
    branch
        .query_selector(".pcdc-button")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert_eq!(read(&host, ".pcdh-clicks"), "1");

    host.query_selector(".pcdh-toggle-show")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert_eq!(
        shown.style().get_property_value("display").unwrap(),
        "",
        "truthy pp-show must restore the component host's stylesheet display",
    );

    host.query_selector(".pcdh-toggle-show")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert_eq!(
        shown.style().get_property_value("display").unwrap(),
        "none",
        "a later false value must hide the same mounted component host again",
    );
    assert!(
        shown.query_selector(".pcdc-button").unwrap().is_some(),
        "pp-show toggles visibility without unmounting the child",
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

#[wasm_bindgen_test]
async fn pp_show_and_style_binding_preserve_each_others_state() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<plan-show-style-host></plan-show-style-host>");
    tick().await;

    let show_first = host
        .query_selector(".pssh-show-first")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    let style_first = host
        .query_selector(".pssh-style-first")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    let bound_display = host
        .query_selector(".pssh-bound-display")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    let display = |el: &HtmlElement| el.style().get_property_value("display").unwrap();
    let color = |el: &HtmlElement| el.style().get_property_value("color").unwrap();
    let click = |selector: &str| {
        host.query_selector(selector)
            .unwrap()
            .unwrap()
            .dyn_into::<HtmlElement>()
            .unwrap()
            .click();
    };

    assert_eq!(display(&show_first), "none");
    assert_eq!(display(&style_first), "none");
    assert_eq!(display(&bound_display), "none");

    // A changed :style value must not remove pp-show's hidden overlay.
    click(".pssh-update");
    tick().await;
    assert_eq!(color(&show_first), "blue");
    assert_eq!(color(&style_first), "blue");
    assert_eq!(color(&bound_display), "blue");
    assert_eq!(display(&show_first), "none");
    assert_eq!(display(&style_first), "none");
    assert_eq!(display(&bound_display), "none");

    // Showing restores the latest style-owned display value, or removes the
    // overlay when the binding did not provide one.
    click(".pssh-toggle");
    tick().await;
    assert_eq!(display(&show_first), "");
    assert_eq!(display(&style_first), "");
    assert_eq!(display(&bound_display), "grid");

    // A visible style update becomes the new value pp-show restores later.
    click(".pssh-update");
    tick().await;
    assert_eq!(display(&bound_display), "flex");
    click(".pssh-toggle");
    tick().await;
    assert_eq!(display(&bound_display), "none");
    click(".pssh-update");
    tick().await;
    assert_eq!(display(&bound_display), "none");
    click(".pssh-toggle");
    tick().await;
    assert_eq!(display(&bound_display), "grid");

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

/// RFC-058 mount-removal slice — child-component host
/// directives inside lifted bodies are installed from
/// `StaticChildMount`, after the child scope exists. This keeps
/// parent prop binds and host listeners out of fallback walk.
#[wasm_bindgen_test]
async fn lifted_child_host_bind_and_listener_install_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-if-host-directive-host")
        .expect("plan-if-host-directive-host registers a template plan");
    let body = plan.if_plans[0]
        .body
        .expect("host directive child body should lift");
    let _ = body;

    let host = mount("<plan-if-host-directive-host></plan-if-host-directive-host>");
    tick().await;

    let toggle = host.query_selector(".pihdh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let label = host
        .query_selector(".phdc-label")
        .unwrap()
        .expect("child label should mount");
    assert_eq!(label.text_content().as_deref(), Some("initial-host-title"));

    label.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        label.text_content().as_deref(),
        Some("host-count-1"),
        "custom-tag @click should dispatch to parent and :label should update child prop",
    );
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 mount-removal slice — child-component `pp-model`
/// inside a lifted body is installed from `StaticChildMount`
/// after the child scope exists. Parent -> child and child ->
/// parent both work without fallback walk.
#[wasm_bindgen_test]
async fn lifted_child_host_model_installs_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-if-model-directive-host")
        .expect("plan-if-model-directive-host registers a template plan");
    let body = plan.if_plans[0]
        .body
        .expect("model directive child body should lift");
    let _ = body;

    let host = mount("<plan-if-model-directive-host></plan-if-model-directive-host>");
    tick().await;

    let toggle = host.query_selector(".pimdh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let child_value = host
        .query_selector(".pmdc-value")
        .unwrap()
        .expect("child model value should mount");
    assert_eq!(
        child_value.text_content().as_deref(),
        Some("initial-model-email"),
        "compiled pp-model:value should mirror parent into child",
    );

    let child = host
        .query_selector("plan-model-directive-child")
        .unwrap()
        .unwrap();
    let child_root = child.first_element_child().unwrap();
    let (child_scope, _) =
        pocopine_core::mount::scope_of_element(&child_root).expect("child scope");
    let args = js_sys::Array::new();
    args.push(&JsValue::from_str("child-model-update"));
    pocopine_core::scope::invoke_handler(child_scope, "set_value", &args);
    tick().await;

    let parent_value = host
        .query_selector(".pimdh-email")
        .unwrap()
        .expect("parent model mirror should mount");
    assert_eq!(
        parent_value.text_content().as_deref(),
        Some("child-model-update"),
        "child model update should emit through compiled host pp-model",
    );
    assert_eq!(
        child_value.text_content().as_deref(),
        Some("child-model-update"),
        "parent mirror should flow back into child without fallback",
    );

    let args = js_sys::Array::new();
    args.push(&JsValue::from_str(""));
    pocopine_core::scope::invoke_handler(child_scope, "set_value", &args);
    tick().await;
    assert_eq!(
        parent_value.text_content().as_deref(),
        Some(""),
        "empty string model updates should not be dropped",
    );
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 mount-removal slice — `pp-as` component templates
/// can bind root-level template attrs/listeners to the hoisted
/// user element without a recursive fallback walk.
#[wasm_bindgen_test]
async fn lifted_pp_as_child_installs_root_plan_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();

    let _child_plan = template_plan_for("plan-as-directive-child")
        .expect("pp-as child should register a root template plan");
    let host_plan = template_plan_for("plan-if-as-directive-host")
        .expect("pp-as host should register a template plan");
    let body = host_plan.if_plans[0]
        .body
        .expect("pp-as child body should lift");
    let _ = body;

    let host = mount("<plan-if-as-directive-host></plan-if-as-directive-host>");
    tick().await;

    let toggle = host.query_selector(".piadh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let button = host
        .query_selector(".piadh-user-button")
        .unwrap()
        .expect("hoisted pp-as button should mount");
    assert_eq!(
        button.get_attribute("data-state").as_deref(),
        Some("idle"),
        "compiled root binding should install on hoisted user element",
    );
    assert!(
        button
            .get_attribute("class")
            .unwrap_or_default()
            .contains("padc-root"),
        "template root class should merge onto hoisted user element",
    );

    button.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        button.get_attribute("data-state").as_deref(),
        Some("active"),
        "compiled root listener should run in the child scope",
    );
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 4.3 — pp-teleport on `<template>` graduates
/// into a `StaticTeleportPlan`. The applier resolves the
/// target selector and clones the template body to it; the
/// portal content lands on `<body>` and the back-link to the
/// origin template is set so consumers can walk back to the
/// host scope.
#[wasm_bindgen_test]
async fn macro_emitted_teleport_plan_drives_pp_teleport_controller() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-teleport-host")
        .expect("plan-teleport-host has one pp-teleport site");
    assert_eq!(plan.teleport_plans.len(), 1);
    assert_eq!(plan.teleport_plans[0].selector, "body");
    // RFC-058 Phase 4.3c — PlanTeleportHost's body is a static
    // `<span>` so it lifts into a body fragment.
    assert!(
        plan.teleport_plans[0].body.is_some(),
        "static-only pp-teleport body must lift into a body fragment",
    );

    let host = mount("<plan-teleport-host></plan-teleport-host>");
    tick().await;

    let body = doc().body().unwrap();
    let portal = body
        .query_selector(".pth-portal")
        .unwrap()
        .expect("teleported content must land in <body>");
    assert_eq!(portal.text_content().as_deref(), Some("teleported-content"));
    // The portal must NOT be inside the host (it was teleported out).
    assert!(host.query_selector(".pth-portal").unwrap().is_none());
    assert_eq!(plan_failure_count(), 0);

    // Manual cleanup so the portal doesn't leak into sibling
    // tests on `body` (the test's mount observer is rooted at
    // `host`, so detaching host doesn't fire release for the
    // inner template — same semantics as today's mount path).
    pocopine_core::mount::release_compiled_subtree(&host);
    host.remove();
    portal.remove();
}

/// RFC-058 Phase 4.2 — pp-for + non-pp-for plan-eligible
/// directive coexisting confirms `for_plans` + `bindings` +
/// row-plan stamping all run on the same template.
#[wasm_bindgen_test]
async fn macro_emitted_for_plan_drives_pp_for_controller() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-for-mixed")
        .expect("plan-for-mixed has both a pp-text and a pp-for");
    assert_eq!(
        plan.for_plans.len(),
        1,
        "the pp-for site lifts into a for-plan entry",
    );
    assert_eq!(plan.for_plans[0].item_name, "row");
    assert_eq!(plan.for_plans[0].items_expr, "rows");
    assert_eq!(plan.for_plans[0].key_expr, Some("row.id"));
    assert!(
        !plan.bindings.is_empty(),
        "the pp-text outside the pp-for keeps registering as a binding",
    );

    let host = mount("<plan-for-mixed></plan-for-mixed>");
    tick().await;

    assert_eq!(read(&host, ".pfm-title"), "mixed-title");
    let rows = host.query_selector_all("li").unwrap();
    assert_eq!(rows.length(), 2);
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 §6.2 layering — template plan + row plan coexist
/// on one template. The title's `pp-text` registers a template
/// plan, the keyed list registers a row plan, and the rendered
/// DOM proves both compilers ran successfully (the title
/// binding fires + the rows mount).
#[wasm_bindgen_test]
async fn template_plan_and_row_plan_coexist_on_one_template() {
    register_all();
    reset_plan_failure_count();

    assert!(
        template_plan_for("plan-for-mixed").is_some(),
        "template plan registers for the pp-text outside the pp-for",
    );

    let host = mount("<plan-for-mixed></plan-for-mixed>");
    tick().await;

    assert_eq!(read(&host, ".pfm-title"), "mixed-title");
    let rows = host.query_selector_all("li").unwrap();
    assert_eq!(rows.length(), 2, "row plan must mount both keyed rows");
    assert_eq!(
        rows.get(0).unwrap().text_content().as_deref(),
        Some("alpha"),
    );
    assert_eq!(rows.get(1).unwrap().text_content().as_deref(), Some("beta"),);

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 3.5a — the slot-fragment runtime hook: a
/// hand-built `SlotSet` passed via
/// `mount_child_component_with_slots` is invoked by
/// `materialize_slot` instead of the legacy capture/replay
/// path. Demonstrates the parent-owned slot fragment ABI
/// end-to-end; Phase 3.5b graduates the macro to emit these
/// fragments automatically.
#[wasm_bindgen_test]
async fn slot_fragment_runtime_hook_replaces_capture_path() {
    use pocopine_core::slot_fragment::{SlotMountCtx, SlotSet};

    register_all();
    reset_plan_failure_count();

    let child_plan = template_plan_for("plan-slot-child")
        .expect("slot-bearing child must register a compiled slot-outlet plan");
    assert_eq!(
        child_plan.slot_outlets.len(),
        1,
        "the child's <slot> is materialised by compiled slot-outlet install",
    );
    assert_eq!(child_plan.slot_outlets[0].name, "default");

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    body.append_child(&host).unwrap();

    let child = doc().create_element("plan-slot-child").unwrap();
    host.append_child(&child).unwrap();

    fn fragment(ctx: SlotMountCtx<'_>) {
        let doc = web_sys::window().unwrap().document().unwrap();
        let span = doc.create_element("span").unwrap();
        span.set_attribute("class", "psc-fragment-marker").unwrap();
        span.set_text_content(Some("from-fragment"));
        ctx.host.append_child(&span).unwrap();
    }

    let slots = SlotSet::new().default_slot(fragment);
    // Hand-built fragment doesn't read parent_proxy — pass dummy
    // (ScopeId(0), JsValue::UNDEFINED). The runtime never derefs
    // them because the static fragment ignores the field.
    pocopine_core::mount::mount_child_component_with_slots(
        &child,
        "plan-slot-child",
        slots,
        pocopine::ScopeId(0),
        &JsValue::UNDEFINED,
    );

    mount_registered_tags(&host);
    tick().await;

    let marker = host
        .query_selector(".psc-fragment-marker")
        .unwrap()
        .expect("fragment-emitted span must replace the child's <slot>");
    assert_eq!(marker.text_content().as_deref(), Some("from-fragment"));

    // The legacy capture path would have left no marker in the
    // DOM (the host had no slot content for the child to capture
    // by name), so finding it here is positive evidence the
    // fragment hook fired.
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 4.1 — the macro lifted `<template pp-if>` out
/// of the runtime mount's directive-dispatch path into a
/// `StaticIfPlan` entry. The applier installs the controller
/// via `if_::install`, the truthy expression toggles the
/// branch in/out, and `plan_failure_count` stays at 0.
#[wasm_bindgen_test]
async fn macro_emitted_if_plan_drives_pp_if_controller() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-if-host")
        .expect("plan-if-host has a plan-eligible template (button + pp-if site)");
    assert_eq!(
        plan.if_plans.len(),
        1,
        "exactly one `<template pp-if>` site",
    );
    assert_eq!(plan.if_plans[0].expr_src, "open");
    // RFC-058 Phase 4.1d — PlanIfHost's body is just a `<span>`
    // with no directives, so it falls inside the v1 lift
    // envelope and the macro emits a body fragment fn.
    assert!(
        plan.if_plans[0].body.is_some(),
        "static-only pp-if body must lift into a body fragment",
    );

    let host = mount("<plan-if-host></plan-if-host>");
    tick().await;

    assert!(
        host.query_selector(".pih-branch").unwrap().is_none(),
        "branch must be absent while `open` is false",
    );

    let toggle = host.query_selector(".pih-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert!(
        host.query_selector(".pih-branch").unwrap().is_some(),
        "branch must mount once `open` flips to true",
    );

    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert!(
        host.query_selector(".pih-branch").unwrap().is_none(),
        "branch must unmount once `open` flips back to false",
    );

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 3.5b — the macro lifted the parent's static
/// slot content into a fragment function and emitted a
/// `StaticSlotFragment` reference on the child-mount entry.
/// At mount time the runtime applier flips onto
/// `mount_child_component_with_slots` and the mount's
/// `materialize_slot` invokes the fragment instead of
/// replaying captured DOM. End-to-end macro-driven slot
/// rendering with no mount auto-discovery in the slot
/// subtree.
#[wasm_bindgen_test]
async fn macro_emitted_slot_fragment_renders_static_slot_content() {
    register_all();
    reset_plan_failure_count();

    let child_plan =
        template_plan_for("plan-slot-child").expect("plan-slot-child has a compiled <slot> outlet");
    assert_eq!(
        child_plan.slot_outlets.len(),
        1,
        "child slot outlet must be explicit in the template plan",
    );
    assert_eq!(child_plan.slot_outlets[0].name, "default");

    let plan = template_plan_for("plan-slot-host-static")
        .expect("plan-slot-host-static is plan-eligible (one nested non-HTML5 tag)");
    assert_eq!(plan.child_mounts.len(), 1, "exactly one child-mount site");
    let child = &plan.child_mounts[0];
    assert_eq!(child.tag, "plan-slot-child");
    assert_eq!(
        child.slots.len(),
        1,
        "static slot subtree must lift into one default fragment",
    );
    assert_eq!(child.slots[0].name, "default");

    let host = mount("<plan-slot-host-static></plan-slot-host-static>");
    tick().await;

    let marker = host
        .query_selector(".pshs-author-marker")
        .unwrap()
        .expect("macro-emitted fragment must stamp the parent-authored span into the child slot");
    assert_eq!(
        marker.text_content().as_deref(),
        Some("macro-emitted-static-fragment"),
    );
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 3 — the parent's plan carries one
/// `StaticChildMount` entry per non-HTML5 tag in its
/// plan-eligible subtree, the runtime applier mounts the leaf
/// child explicitly via `mount_child_component`, and the
/// mount's `__pp_mounted` guard turns its subsequent
/// auto-discovery into a no-op. Net effect today is parity
/// with the mount-driven path; the test pins the structural
/// contract so Phase 6 can drop mount discovery without
/// regression.
#[wasm_bindgen_test]
async fn parent_plan_drives_child_mount_via_static_child_mount() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-child-host")
        .expect("plan-child-host has a plan-eligible template — one nested non-HTML5 tag");
    assert_eq!(
        plan.child_mounts.len(),
        1,
        "exactly one `<plan-child-leaf>` site",
    );
    assert_eq!(plan.child_mounts[0].tag, "plan-child-leaf");

    let host = mount("<plan-child-host></plan-child-host>");
    tick().await;

    let leaf_text = read(&host, ".pcl-leaf");
    assert_eq!(
        leaf_text, "leaf-mounted",
        "the leaf must mount and bind its own plan via the parent's child-mount entry",
    );
    assert_eq!(
        plan_failure_count(),
        0,
        "child-mount install must not trip the fail-fast counter",
    );

    host.remove();
}

/// RFC-058 Phase 3.5f — the macro partitions a custom-tag's
/// slot children into one default subtree + N named-slot
/// subtrees and lifts each independently into a slot fragment.
/// The child-mount entry carries one `(name, fragment)` pair
/// per lifted slot; the runtime `materialize_slot` resolves
/// each by name through `SlotSet::lookup` rather than the
/// legacy capture path.
#[wasm_bindgen_test]
async fn macro_emits_named_slot_fragment() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-named-slot-host")
        .expect("plan-named-slot-host has one nested non-HTML5 tag");
    assert_eq!(plan.child_mounts.len(), 1, "exactly one child-mount site");
    let child = &plan.child_mounts[0];
    assert_eq!(child.tag, "plan-named-slot-child");
    assert_eq!(
        child.slots.len(),
        3,
        "default + header + footer subtrees must each lift into their own fragment",
    );
    let names: Vec<&str> = child.slots.iter().map(|s| s.name).collect();
    assert!(names.contains(&"default"));
    assert!(names.contains(&"header"));
    assert!(names.contains(&"footer"));

    let host = mount("<plan-named-slot-host></plan-named-slot-host>");
    tick().await;

    let header = host
        .query_selector(".pnsh-header-content")
        .unwrap()
        .expect("header slot must mount via the lifted fragment");
    assert_eq!(
        header.text_content().as_deref(),
        Some("macro-emitted-header")
    );

    let body = host
        .query_selector(".pnsh-default-content")
        .unwrap()
        .expect("default content must mount via the lifted fragment");
    assert_eq!(
        body.text_content().as_deref(),
        Some("macro-emitted-default")
    );

    let footer = host
        .query_selector(".pnsh-footer-content")
        .unwrap()
        .expect("footer slot must mount via the lifted fragment");
    assert_eq!(
        footer.text_content().as_deref(),
        Some("macro-emitted-footer")
    );

    // Pin: the lifted named slots take the same compiled path as
    // the default lift — no mount fallback for any slot.
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 3.5g — `<template pp-slot="N" pp-let="ident">`
/// lifts into a slot fragment with `scoped_let = Some("ident")`.
/// The runtime materialiser builds a `SlotScope` from the
/// child's `<slot :prop="path">` bindings and invokes the
/// fragment against that scope, so `pp-text="ident.field"`
/// resolves through SlotScope's RFC-011 routing without falling
/// back to the legacy mount capture path.
#[wasm_bindgen_test]
async fn macro_lifts_scoped_slot_fragment_with_pp_let() {
    register_all();
    reset_plan_failure_count();
    #[cfg(any(debug_assertions, feature = "devtools"))]
    let scopes_before = pocopine_core::scope::Scope::count();

    let plan = template_plan_for("plan-scoped-slot-host")
        .expect("plan-scoped-slot-host has one nested non-HTML5 tag");
    assert_eq!(plan.child_mounts.len(), 1, "exactly one child-mount site");
    let child = &plan.child_mounts[0];
    assert_eq!(child.slots.len(), 1, "the row slot lifts");
    assert_eq!(child.slots[0].name, "row");
    assert_eq!(
        child.slots[0].scoped_let,
        Some("ctx"),
        "pp-let identifier must propagate to StaticSlotFragment.scoped_let",
    );

    let host = mount("<plan-scoped-slot-host></plan-scoped-slot-host>");
    tick().await;

    // Empty initial label — the SlotScope routes `ctx.label`
    // through the child's `current.label`, which is "" by default.
    let row = host
        .query_selector(".pssh-row")
        .unwrap()
        .expect("scoped slot row must mount via the lifted fragment");
    assert_eq!(row.text_content().as_deref(), Some(""));

    // Mutate the child's `current` — the slot scope's bind_source
    // is the child's proxy, so the effect re-fires.
    let child_tag = host
        .query_selector("plan-scoped-slot-child")
        .unwrap()
        .unwrap();
    let child_root = child_tag.first_element_child().unwrap();
    let (_id, child_proxy) =
        pocopine_core::mount::scope_of_element(&child_root).expect("child scope");
    let next = serde_wasm_bindgen::to_value(&PsscRow {
        label: "scoped-from-fragment".into(),
    })
    .unwrap();
    js_sys::Reflect::set(&child_proxy, &"current".into(), &next).unwrap();
    tick().await;

    let row = host
        .query_selector(".pssh-row")
        .unwrap()
        .expect("scoped slot row must still be mounted after update");
    assert_eq!(
        row.text_content().as_deref(),
        Some("scoped-from-fragment"),
        "ctx.label must reactively re-resolve when the child's `current` changes",
    );

    // Codex review fix: dispatch a handler from inside the scoped
    // slot. The button's `@click="bump"` resolves through
    // SlotScope's invoke fall-through to the parent's handler;
    // without that delegation the click would silently no-op.
    let bump = host.query_selector(".pssh-bump").unwrap().unwrap();
    bump.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(
        read(&host, ".pssh-mark"),
        "bumped-from-scoped-slot",
        "@click inside a scoped slot must invoke the parent handler via SlotScope::invoke fall-through",
    );

    assert_eq!(plan_failure_count(), 0);

    #[cfg(any(debug_assertions, feature = "devtools"))]
    {
        pocopine_core::mount::release_compiled_subtree(&host);
        assert_eq!(
            pocopine_core::scope::Scope::count(),
            scopes_before,
            "scoped-slot materialization must release its borrowed SlotScope"
        );
    }
    host.remove();
}

/// RFC-058 Phase 3 hardening — `pp-roving.both` lifts into a
/// `StaticOpaqueDirective` entry instead of preserving the attr
/// on the cleaned HTML and forcing requires_walker. The applier
/// dispatches it through the runtime registry after slot
/// materialisation, so the container's items are in the DOM
/// when roving's `query_items` runs.
#[wasm_bindgen_test]
async fn macro_lifts_opaque_runtime_directive() {
    register_all();
    reset_plan_failure_count();
    #[cfg(any(debug_assertions, feature = "devtools"))]
    let listeners_before = pocopine_core::mount::listener_count();

    let plan = template_plan_for("plan-opaque-directive-host")
        .expect("plan-opaque-directive-host registers a template plan");
    assert_eq!(
        plan.opaque_directives.len(),
        1,
        "the pp-roving.both attr must lift into one opaque-directive entry",
    );
    let d = &plan.opaque_directives[0];
    assert_eq!(d.name, "roving");
    assert_eq!(d.arg, None);
    assert_eq!(d.modifiers, &["both"]);

    let host = mount("<plan-opaque-directive-host></plan-opaque-directive-host>");
    tick().await;

    // The roving controller stamps tabindex on the items —
    // first item gets `tabindex="0"`, the rest get `-1`. This
    // proves the dispatch reached `roving::run` against the
    // post-slot-materialisation DOM.
    let items = host.query_selector_all(".podh-item").unwrap();
    assert_eq!(items.length(), 3);
    assert_eq!(
        items
            .get(0)
            .unwrap()
            .dyn_ref::<HtmlElement>()
            .unwrap()
            .get_attribute("tabindex")
            .as_deref(),
        Some("0"),
        "first roving item should be the tabstop",
    );
    assert_eq!(
        items
            .get(1)
            .unwrap()
            .dyn_ref::<HtmlElement>()
            .unwrap()
            .get_attribute("tabindex")
            .as_deref(),
        Some("-1"),
    );

    assert_eq!(plan_failure_count(), 0);

    #[cfg(any(debug_assertions, feature = "devtools"))]
    {
        assert!(
            pocopine_core::mount::listener_count() > listeners_before,
            "pp-roving must retain its keydown closure in the releasable listener table"
        );
        pocopine_core::mount::release_compiled_subtree(&host);
        assert_eq!(
            pocopine_core::mount::listener_count(),
            listeners_before,
            "pp-roving listener must be removed and its closure dropped on teardown"
        );
    }
    host.remove();
}

/// RFC-058 Phase 3 hardening — drift guard between the macro's
/// opaque-directive allowlist and the runtime directive
/// registry. The macro emits `StaticOpaqueDirective` entries
/// trusting that `directives::lookup(name)` resolves at apply
/// time; if a directive is removed from the registry without
/// updating the macro, every fixture using that directive trips
/// `plan_failure_count` silently. This test makes that drift
/// loud at compile time by asserting each allowlisted name has
/// a live runtime handler.
///
/// When you add or remove an entry from
/// `is_lift_eligible_opaque` in
/// `crates/pocopine-macros/src/template_plan.rs`, mirror the
/// change here. RFC-058 Phase 6.5 — the directive registry is
/// gone; the compiled plan dispatcher is the typed
/// match that replaces it. We can no longer probe lookup, so
/// just keep the allowlist as documentation.
#[wasm_bindgen_test]
async fn opaque_lift_allowlist_documented() {
    const OPAQUE_LIFT_ELIGIBLE: &[&str] = &["roving", "resize", "intersect", "anchor", "flip"];
    assert_eq!(OPAQUE_LIFT_ELIGIBLE.len(), 5);
}

/// RFC-058 Phase 3 hardening — `pp-ref` on a custom-host
/// element must lift into a regular ref entry instead of
/// preserving the attr and forcing requires_walker. The runtime
/// semantic matches the native-element case: register the host
/// DOM element under the given name in the parent's ref table.
#[wasm_bindgen_test]
async fn macro_lifts_pp_ref_on_custom_child_host() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-child-host-ref-host")
        .expect("plan-child-host-ref-host registers a template plan");
    assert_eq!(
        plan.refs.len(),
        1,
        "pp-ref on the custom host must lift into a single ref entry",
    );
    assert_eq!(plan.refs[0].name, "leaf");
    assert_eq!(plan.child_mounts.len(), 1);

    let host = mount("<plan-child-host-ref-host></plan-child-host-ref-host>");
    tick().await;

    // The ref is registered against the host's scope at mount
    // time. Resolve it via `scope_of_element` + `refs::get_on`
    // so the assertion doesn't depend on `current_scope_id`
    // ambient state, which dies after `tick`.
    let host_tag = host
        .query_selector("plan-child-host-ref-host")
        .unwrap()
        .unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (host_scope_id, _) =
        pocopine_core::mount::scope_of_element(&host_root).expect("host scope");
    let leaf_via_ref =
        pocopine::refs::get_on(host_scope_id, "leaf").expect("ref `leaf` must resolve");
    assert_eq!(
        leaf_via_ref.local_name(),
        "plan-child-leaf",
        "the resolved ref must point at the custom-host element itself, matching the mount semantic",
    );
    // Fallthrough sanity: the host's `class="pchrh-leaf"` rides
    // through RFC-010 author-class forwarding onto the leaf
    // template's root `<span>`. Same as the mount path — the
    // pp-ref lift must not change that semantic.
    let leaf_root = leaf_via_ref
        .first_element_child()
        .expect("leaf component must mount its template root");
    let class = leaf_root.get_attribute("class").unwrap_or_default();
    assert!(
        class.contains("pchrh-leaf"),
        "host's class must fall through onto the leaf root via RFC-010 forwarding (got `{class}`)",
    );
    assert!(
        class.contains("pcl-leaf"),
        "the leaf template's own class must be preserved alongside (got `{class}`)",
    );

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 6.2 — `{{expr}}` text interpolation lifts into
/// `StaticInterp` plan entries. The applier installs effects
/// per dynamic segment using the same install path the runtime
/// scanner produced.
#[wasm_bindgen_test]
async fn macro_lifts_text_interpolation() {
    register_all();
    reset_plan_failure_count();

    let plan =
        template_plan_for("plan-interp-host").expect("plan-interp-host registers a template plan");
    assert_eq!(
        plan.interps.len(),
        2,
        "two text-node siblings carry interpolation; the third is pure static and skips lifting",
    );
    // Static-mixed line: 5 segments — "hello ", `{{label}}`,
    // ", you have ", `{{count}}`, " items".
    let mixed = plan
        .interps
        .iter()
        .find(|i| i.segments.len() == 5)
        .expect("mixed-line interp entry");
    assert_eq!(mixed.text_index, 0);
    // Bare interp: 1 segment — `{{label}}`.
    let bare = plan
        .interps
        .iter()
        .find(|i| i.segments.len() == 1)
        .expect("bare interp entry");
    assert_eq!(bare.text_index, 0);

    let host = mount("<plan-interp-host></plan-interp-host>");
    tick().await;

    assert_eq!(read(&host, ".pih-line"), "hello world, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "world");
    assert_eq!(read(&host, ".pih-static"), "no interp here");

    // Reactive update flows through the planned segment.
    let host_tag = host.query_selector("plan-interp-host").unwrap().unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) = pocopine_core::mount::scope_of_element(&host_root).expect("host scope");
    js_sys::Reflect::set(&host_proxy, &"label".into(), &"there".into()).unwrap();
    tick().await;
    assert_eq!(read(&host, ".pih-line"), "hello there, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "there");

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 6.2 regression — when a single parent carries
/// two `{{expr}}` text nodes separated by an element, the
/// macro emits both entries with `text_index` keyed against
/// the original DOM (0 and 1). compiled interp install walks the
/// entries in order, but each `install_planned` mutates the
/// parent's live text-node list — inserting static + dynamic
/// siblings before the placeholder, then removing the
/// placeholder. After the first install completes, what was
/// `text_index = 1` in the macro's view (the original
/// "b {{y}}" text) is no longer the second text-node child;
/// the dynamic node injected by the first install now occupies
/// that slot. Without a snapshot or a reverse-order traversal,
/// the second install targets the wrong text node, the y
/// binding clobbers the x binding, and the original
/// "b {{y}}" stays in the DOM as literal text.
///
/// Asserts both interpolations land in the right slots and
/// reactively update independently.
#[wasm_bindgen_test]
async fn planned_interp_keeps_text_indexes_valid_across_mutations() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<plan-interp-multi-host></plan-interp-multi-host>");
    tick().await;

    let line_text = read(&host, ".pimh-line");
    assert_eq!(
        line_text, "a Xmiddleb Y",
        "both interp entries must resolve against the right text slots",
    );

    let host_tag = host
        .query_selector("plan-interp-multi-host")
        .unwrap()
        .unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) = pocopine_core::mount::scope_of_element(&host_root).expect("host scope");

    js_sys::Reflect::set(&host_proxy, &"x".into(), &"NEW_X".into()).unwrap();
    js_sys::Reflect::set(&host_proxy, &"y".into(), &"NEW_Y".into()).unwrap();
    tick().await;

    assert_eq!(
        read(&host, ".pimh-line"),
        "a NEW_Xmiddleb NEW_Y",
        "both bindings must remain independent — y must not have clobbered x",
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

/// RFC-058/RFC-061 — compiled root discovery mounts every
/// registered component tag inside `root` via the compiled path
/// without binding the wrapper or any non-component descendant.
/// The helper resolves registered tags via a single
/// `query_selector_all`, then routes each through
/// `mount_child_component` directly. `bind` itself is never
/// invoked on the wrapper, the intermediate `<section>`, or the
/// component tag. The plan applier handles every directive on every
/// descendant via compiled plan entries.
///
/// Pin the bind-call delta of 0 so any regression that
/// re-introduces a body-level recursive scan is loud.
#[wasm_bindgen_test]
async fn compiled_root_discovery_skips_runtime_recursion_for_registered_tags() {
    register_all();
    reset_plan_failure_count();

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_attribute("class", "scsr-wrapper").unwrap();
    host.set_inner_html(
        r#"<section class="scsr-section">
              <plan-interp-host></plan-interp-host>
           </section>"#,
    );
    body.append_child(&host).unwrap();
    mount_registered_tags(&host);
    tick().await;

    assert_eq!(read(&host, ".pih-line"), "hello world, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "world");
    assert_eq!(read(&host, ".pih-static"), "no interp here");

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 6.5 — `pp-model` on a native input now lifts
/// into a [`StaticNativeModel`] entry on the template plan
/// instead of needing a runtime fallback. The compiled mount path
/// installs the read-side effect + write-side listener directly
/// via `directives::model::install_native`; no fallback runs.
/// Pin the input's reactive end-to-end behaviour to lock the
/// lift.
#[wasm_bindgen_test]
async fn pp_model_on_native_input_lifts_without_walker_fallback() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("start-compiled-model-host")
        .expect("start-compiled-model-host registers a template plan");
    assert_eq!(
        plan.native_models.len(),
        1,
        "the fixture's single <input pp-model> must produce one StaticNativeModel entry",
    );

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<start-compiled-model-host></start-compiled-model-host>");
    body.append_child(&host).unwrap();
    mount_registered_tags(&host);
    tick().await;

    assert_eq!(read(&host, ".scmh-readout"), "seed");
    assert_eq!(plan_failure_count(), 0);

    let input = host
        .query_selector(".scmh-input")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlInputElement>()
        .unwrap();
    input.set_value("typed");
    let event = web_sys::Event::new("input").unwrap();
    input.dispatch_event(&event).unwrap();
    tick().await;

    assert_eq!(
        read(&host, ".scmh-readout"),
        "typed",
        "lifted pp-model must wire input → scope through the compiled path",
    );

    host.remove();
}

/// RFC-058 Phase 6.3 — the compiled entry applies the entire
/// plan, then descends via `finalize_compiled_subtree`
/// (lifecycle-only) instead of re-binding every descendant.
///
/// `PlanInterpHost` is a non-trivial plan-clean fixture: 1
/// outer test-harness `<div>` (the `mount()` helper's wrapper),
/// the `<plan-interp-host>` component tag, then a template root
/// and 3 native children inside. The compiled entry binds none of
/// them; it routes through compiled plan entries and
/// `finalize_compiled_subtree`.
#[wasm_bindgen_test]
async fn compiled_mount_skips_recursion_for_plan_clean_subtrees() {
    register_all();

    let host = mount("<plan-interp-host></plan-interp-host>");
    tick().await;

    // The plan applied correctly — sanity-check the
    // rendered DOM and a reactive update.
    assert_eq!(read(&host, ".pih-line"), "hello world, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "world");

    let host_tag = host.query_selector("plan-interp-host").unwrap().unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) = pocopine_core::mount::scope_of_element(&host_root).expect("host scope");
    js_sys::Reflect::set(&host_proxy, &"label".into(), &"phase6".into()).unwrap();
    tick().await;
    assert_eq!(read(&host, ".pih-bare"), "phase6");

    host.remove();
}

// ─── RFC-095 W0 — keyed fast-path symmetry gates ────────────────
//
// The keyed reconciler has four fast paths (append, prepend,
// single-remove, two-swap) that each reimplement a slice of the
// general path's semantics. This gate drives a mutation whose
// SHAPE selects each fast path, plus general-path shapes
// (reorder+insert+remove, full replace, clear, rebuild), and
// oracle-checks the rendered row order against the list after
// every step. A fast path that is *almost* equivalent to the
// general path fails here, not in an app.

#[wasm_bindgen_test]
async fn keyed_fast_paths_match_list_oracle() {
    let host = mount("<rfc064-keyed-fast-host></rfc064-keyed-fast-host>");
    tick().await;

    let root = host
        .query_selector("rfc064-keyed-fast-host")
        .unwrap()
        .unwrap()
        .first_element_child()
        .unwrap();
    let (_id, proxy) = pocopine_core::mount::scope_of_element(&root).expect("host scope");

    let write_rows = |rows: &[(u32, &str)]| {
        let arr = js_sys::Array::new();
        for (id, label) in rows {
            let o = js_sys::Object::new();
            js_sys::Reflect::set(&o, &"id".into(), &JsValue::from_f64(*id as f64)).unwrap();
            js_sys::Reflect::set(&o, &"label".into(), &JsValue::from_str(label)).unwrap();
            arr.push(&o);
        }
        js_sys::Reflect::set(&proxy, &"rows".into(), &arr).unwrap();
    };
    let read_rows = || -> Vec<String> {
        let nodes = host.query_selector_all(".r64k-row").unwrap();
        (0..nodes.length())
            .filter_map(|i| nodes.get(i).and_then(|n| n.text_content()))
            .collect()
    };

    // (shape that selects the path, expected labels in order)
    let steps: &[(&str, &[(u32, &str)])] = &[
        // on_setup seeds one/two/three — append fast path:
        (
            "append",
            &[(1, "one"), (2, "two"), (3, "three"), (4, "four")],
        ),
        // prepend fast path:
        (
            "prepend",
            &[
                (0, "zero"),
                (1, "one"),
                (2, "two"),
                (3, "three"),
                (4, "four"),
            ],
        ),
        // single-remove fast path (drop id 2):
        (
            "single-remove",
            &[(0, "zero"), (1, "one"), (3, "three"), (4, "four")],
        ),
        // two-swap fast path (swap zero <-> four):
        (
            "two-swap",
            &[(4, "four"), (1, "one"), (3, "three"), (0, "zero")],
        ),
        // general path: reorder + insert + remove in one shot:
        (
            "general",
            &[(3, "three"), (9, "nine"), (0, "zero"), (1, "one")],
        ),
        // general path: same keys, every label rewritten:
        ("relabel", &[(3, "III"), (9, "IX"), (0, "0"), (1, "I")]),
        // clear:
        ("clear", &[]),
        // rebuild from empty (cold pool):
        ("rebuild", &[(7, "seven"), (8, "eight")]),
    ];

    for (name, rows) in steps {
        write_rows(rows);
        tick().await;
        let expected: Vec<String> = rows.iter().map(|(_, l)| l.to_string()).collect();
        assert_eq!(
            read_rows(),
            expected,
            "fast-path symmetry violated at step `{name}`",
        );
    }

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

// ─── RFC-095 W3b — plan-gated lazy proxy minting ────────────────

/// A bindings/interps-only plan must mount WITHOUT a proxy (no
/// trap closures, no `Proxy` per instance), still render through
/// the scoped readers, and lazy-mint the proxy on first dynamic
/// need — the RFC-054 row contract, generalized to components.
#[wasm_bindgen_test]
async fn proxy_elided_component_renders_and_lazy_mints() {
    let host = mount("<plan-text-echo></plan-text-echo>");
    tick().await;

    // The plan must have been classified proxy-free…
    let plan = template_plan_for("plan-text-echo").expect("plan registered");
    assert!(
        !plan.needs_proxy,
        "a pp-text-only plan must classify as proxy-free",
    );

    // …the component must render (scoped reader, no proxy)…
    let root = host
        .query_selector("plan-text-echo")
        .unwrap()
        .unwrap()
        .first_element_child()
        .unwrap();
    let msg = host.query_selector(".ple-msg").unwrap().unwrap();
    assert_eq!(
        msg.text_content().as_deref(),
        Some("value: {count} ok"),
        "elided component must render via the scoped reader",
    );

    // …with NO proxy minted at mount…
    assert!(
        !pocopine_core::mount::has_minted_proxy(&root),
        "eligible plan must not mint a proxy at mount",
    );

    // …and the first dynamic consumer lazy-mints it.
    let (_id, proxy) = pocopine_core::mount::scope_of_element(&root).expect("lazy mint");
    assert!(!proxy.is_undefined());
    assert!(
        pocopine_core::mount::has_minted_proxy(&root),
        "scope_of_element must mint + cache the proxy on demand",
    );

    // The lazily-minted proxy is fully functional: a write through
    // its set trap re-renders the elided binding — and (RFC-096
    // S3) the scalar update rides the typed text lane: zero serde
    // projections are built for it.
    let serde_before = pocopine_core::scope::serde_projection_count();
    js_sys::Reflect::set(&proxy, &"message".into(), &JsValue::from_str("fresh")).unwrap();
    tick().await;
    assert_eq!(msg.text_content().as_deref(), Some("fresh"));
    assert_eq!(
        pocopine_core::scope::serde_projection_count(),
        serde_before,
        "S3: a scalar pp-text update must build no serde projection",
    );

    // RFC-096 S2 — listeners + native models are access-driven
    // now, so a flat interactive component (input + pp-model +
    // readout) is ALSO proxy-free…
    let interactive = template_plan_for("start-compiled-model-host").expect("plan registered");
    assert!(
        !interactive.needs_proxy,
        "S2: listener/model-only plans must classify proxy-free",
    );
    // …and (RFC-094 Phase 2) cond chains with proxy-free bodies
    // are eligible too — the access-based controller closed the
    // pp-if elision tail…
    let chain = template_plan_for("plan-if-body-host").expect("plan registered");
    assert!(
        !chain.needs_proxy,
        "RFC-094: a cond plan with proxy-free branch bodies must elide",
    );
    // …and (RFC-094 Phase 4) pp-for plans elide too when the items
    // expression isn't `$`-rooted and the pp-key (if any) is
    // item-rooted — rows bind per-row LoopScopes, never the parent
    // proxy.
    let structural = template_plan_for("plan-for-body-host").expect("plan registered");
    assert!(
        !structural.needs_proxy,
        "RFC-094 Phase 4: an item-rooted pp-for plan must elide the parent proxy",
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

// ─── RFC-096 S4 — proxy endgame ─────────────────────────────────

/// The S4 acceptance gates: an elided component's mount mints
/// ZERO proxies; `js_bridge` is the one explicit way to get one,
/// it memoizes, and its traps ride the shared read/write mirrors.
#[wasm_bindgen_test]
async fn elided_mount_mints_nothing_and_js_bridge_is_explicit() {
    register_all();
    let mints_before = pocopine_core::scope::proxies_minted_count();
    let host = mount("<plan-text-echo></plan-text-echo>");
    tick().await;
    assert_eq!(
        pocopine_core::scope::proxies_minted_count(),
        mints_before,
        "S4: mounting an elided component must mint zero proxies",
    );

    let root = host
        .query_selector("plan-text-echo")
        .unwrap()
        .unwrap()
        .first_element_child()
        .unwrap();
    let scope_id = {
        // Resolve the id WITHOUT scope_of_element (which would
        // lazy-mint): the host stamp carries it.
        let raw = host.query_selector("plan-text-echo").unwrap().unwrap();
        pocopine_core::mount::child_component_scope_id(&raw).expect("stamped id")
    };

    // js_bridge: mints exactly once, memoizes, and is live.
    let b1 = pocopine_core::scope::js_bridge(scope_id).expect("bridge");
    let b2 = pocopine_core::scope::js_bridge(scope_id).expect("bridge");
    assert_eq!(
        pocopine_core::scope::proxies_minted_count(),
        mints_before + 1,
        "js_bridge must mint exactly once",
    );
    assert!(js_sys::Object::is(&b1, &b2), "js_bridge must memoize");

    js_sys::Reflect::set(&b1, &"message".into(), &JsValue::from_str("via bridge")).unwrap();
    tick().await;
    assert_eq!(
        root.query_selector(".ple-msg")
            .unwrap()
            .unwrap()
            .text_content()
            .as_deref(),
        Some("via bridge"),
        "bridge writes ride the shared write mirror",
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

// ─── RFC-094 Phase 0 — structural templates carry `hidden` ─────

/// Stylekit's space-*/divide-* utilities select
/// `> :not([hidden]) ~ :not([hidden])`; a structural `<template>`
/// is an element WITHOUT the hidden attribute, so it used to
/// count as a phantom sibling (margins/borders around unmounted
/// branches). Phase 0 stamps `hidden` in the cleaned HTML.
#[wasm_bindgen_test]
async fn structural_templates_are_hidden_stamped() {
    // cond/match/for templates are comment-swapped at install
    // (RFC-094 Phases 2–4); pp-teleport templates remain live-DOM
    // anchors (origin back-link + stash), so the stamp is checked
    // on a teleport fixture.
    let host = mount("<plan-teleport-host></plan-teleport-host>");
    tick().await;
    let tpl = host
        .query_selector("template")
        .unwrap()
        .expect("the pp-teleport template anchor is in the live DOM");
    assert!(
        tpl.has_attribute("hidden"),
        "structural template anchors must carry `hidden` so sibling \
         selectors skip them",
    );
    host.remove();
}

// ─── RFC-094 Phase 4 — pp-for comment anchor ────────────────────

/// The for controller swaps its `<template>` for `<!--pp:for-->`
/// at install: rows are the only element children of the list
/// parent, so the `:nth-child` family (and `last:` Stylekit
/// variants) finally line up with what's visible — the phantom
/// trailing template used to break `li:last-child` outright.
#[wasm_bindgen_test]
async fn for_swaps_template_for_comment_anchor() {
    register_all();
    let host = mount("<plan-for-body-host></plan-for-body-host>");
    tick().await;

    assert_eq!(
        host.query_selector_all("template").unwrap().length(),
        0,
        "the pp-for template must leave the live DOM",
    );
    let rows = host.query_selector_all(".pfbh-row").unwrap();
    assert_eq!(rows.length(), 2);

    // The comment anchor sits AFTER the rows (clones insert before
    // it) and is invisible to CSS structural pseudo-classes.
    let last = host
        .query_selector("ul > li:last-child")
        .unwrap()
        .expect(":last-child must match the final row, not bail on the anchor");
    assert_eq!(last.text_content().as_deref(), Some("beta"));
    let second = host
        .query_selector("ul > li:nth-child(2)")
        .unwrap()
        .expect(":nth-child must count only live rows");
    assert_eq!(second.text_content().as_deref(), Some("beta"));

    // Reconciliation keeps inserting at the anchor.
    let add = host.query_selector(".pfbh-add").unwrap().unwrap();
    add.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    let last = host
        .query_selector("ul > li:last-child")
        .unwrap()
        .expect("anchor still positions appended rows");
    assert_eq!(last.text_content().as_deref(), Some("row-3"));
    assert_eq!(host.query_selector_all(".pfbh-row").unwrap().length(), 3);

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

// ─── RFC-094 Phase 2 — conditional chains ───────────────────────

/// The chain contract end-to-end: one plan entry per chain,
/// consumed member templates leave the live DOM, the head swaps
/// for a comment anchor, exactly one branch renders, switching
/// follows first-truthy order, and a chain with proxy-free
/// bodies mounts elided.
#[wasm_bindgen_test]
async fn cond_chain_renders_exactly_one_branch_and_switches() {
    register_all();
    let plan = template_plan_for("plan-cond-chain-host").expect("plan registered");
    assert_eq!(plan.if_plans.len(), 1, "one chain = one plan entry");
    let chain = &plan.if_plans[0];
    assert_eq!(chain.else_if.len(), 1);
    assert!(chain.has_else);
    assert_eq!(chain.consumed_count, 2);
    assert!(
        !plan.needs_proxy,
        "chain with proxy-free bodies + listener must elide (RFC-094/096 convergence)",
    );

    let mints_before = pocopine_core::scope::proxies_minted_count();
    let host = mount("<plan-cond-chain-host></plan-cond-chain-host>");
    tick().await;
    assert_eq!(
        pocopine_core::scope::proxies_minted_count(),
        mints_before,
        "chain mount must mint zero proxies",
    );

    let count_all = || host.query_selector_all(".pcc-branch").unwrap().length();
    let text_of_active = || {
        host.query_selector(".pcc-branch")
            .unwrap()
            .map(|e| e.text_content().unwrap_or_default())
            .unwrap_or_default()
    };
    // count = 0 → else branch, exactly one clone, no templates in
    // the live DOM (head comment-swapped, members detached).
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "zero");
    assert_eq!(
        host.query_selector_all("template").unwrap().length(),
        0,
        "chain templates must leave the live DOM",
    );

    // Drive the chain through every branch via the handler.
    let bump = host.query_selector(".pcc-bump").unwrap().unwrap();
    let click = |el: &Element| {
        el.dyn_ref::<HtmlElement>().unwrap().click();
    };

    click(&bump); // count = 1 → small
    tick().await;
    assert_eq!(count_all(), 1, "exactly one branch after switch");
    assert_eq!(text_of_active(), "small");

    for _ in 0..5 {
        click(&bump); // count = 6 → big
    }
    tick().await;
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "big");

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

#[wasm_bindgen_test]
async fn match_dispatches_arms_and_updates_payload_in_place() {
    register_all();
    let plan = template_plan_for("plan-match-host").expect("plan registered");
    assert_eq!(plan.match_plans.len(), 1, "one pp-match = one plan entry");
    let mp = &plan.match_plans[0];
    assert_eq!(mp.cases.len(), 3);
    assert_eq!(mp.cases[0].tags, &["Idle", "Loading"]);
    assert_eq!(mp.cases[1].tags, &["Ready"]);
    assert_eq!(mp.cases[1].bind_name, Some("msg"));
    assert!(mp.cases[2].tags.is_empty(), "`_` arm has no tags");
    assert!(
        !plan.needs_proxy,
        "match with proxy-free arm bodies must elide (RFC-094/096 convergence)",
    );

    let mints_before = pocopine_core::scope::proxies_minted_count();
    let host = mount("<plan-match-host></plan-match-host>");
    tick().await;
    assert_eq!(
        pocopine_core::scope::proxies_minted_count(),
        mints_before,
        "match mount must mint zero proxies",
    );

    let count_all = || host.query_selector_all(".pm-arm").unwrap().length();
    let active = || host.query_selector(".pm-arm").unwrap().unwrap();
    let text_of_active = || active().text_content().unwrap_or_default();

    // Idle → first arm, exactly one clone, no live templates
    // (the match template swapped for its comment anchor).
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "pending");
    assert_eq!(
        host.query_selector_all("template").unwrap().length(),
        0,
        "match + case templates must leave the live DOM",
    );

    let next = host.query_selector(".pm-next").unwrap().unwrap();
    let click = |el: &Element| {
        el.dyn_ref::<HtmlElement>().unwrap().click();
    };

    // Idle → Loading: SAME arm (`Idle | Loading`) — no remount.
    let pending_el = active();
    click(&next);
    tick().await;
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "pending");
    assert!(
        active().is_same_node(Some(pending_el.as_ref())),
        "same-arm switch must not remount the clone",
    );

    // Loading → Ready("one"): arm switch, pp-let payload bound.
    click(&next);
    tick().await;
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "one");

    // Ready("one") → Ready("two"): same tag — payload updates IN
    // PLACE through the PayloadScope, no remount.
    let ready_el = active();
    click(&next);
    tick().await;
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "two");
    assert!(
        active().is_same_node(Some(ready_el.as_ref())),
        "same-tag payload change must not remount the clone",
    );

    // Ready("two") → Err { code: 7 }: falls to the `_` wildcard.
    click(&next);
    tick().await;
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "other");

    // Err → Idle: full cycle back to the first arm.
    click(&next);
    tick().await;
    assert_eq!(count_all(), 1);
    assert_eq!(text_of_active(), "pending");

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}
