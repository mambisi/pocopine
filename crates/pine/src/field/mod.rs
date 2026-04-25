//! `<pine-field-*>` — accessible form field (compound).
//!
//! Mirrors Base UI's `<Field.*>`. Five parts:
//!
//! - **Root** (`pine-field-root`) — owns `name`, `disabled`,
//!   `invalid`, and the interaction flags `touched`, `dirty`,
//!   `filled`, `focused`. Generates the shared `control_id` /
//!   `description_id` / `error_id` and provides its Handle so
//!   descendants can read them without the author wiring ids.
//! - **Label** (`pine-field-label`) — native `<label>` whose
//!   `for` targets the Control's generated id. Mirrors Root's
//!   state onto `data-*` attributes.
//! - **Control** (`pine-field-control`) — `display:contents`
//!   wrapper around an author-supplied `<input>` / `<textarea>` /
//!   `<select>`. Stamps the inner element with the shared id,
//!   `aria-describedby`, `aria-invalid`, and `aria-errormessage`,
//!   and installs focus / blur / input listeners that feed
//!   Root's state flags. `invalid` events from native constraint
//!   validation also flip `Root.invalid`.
//! - **Description** (`pine-field-description`) — `<p>` with the
//!   shared `description_id` so the Control picks it up in
//!   `aria-describedby`.
//! - **Error** (`pine-field-error`) — `<p role="alert">` shown
//!   only when `Root.invalid`. Carries the shared `error_id` so
//!   the Control references it in `aria-errormessage`.
//!
//! ```html
//! <pine-field-root name="fullname">
//!   <pine-field-label>Full name</pine-field-label>
//!   <pine-field-control>
//!     <input type="text" required placeholder="Required">
//!   </pine-field-control>
//!   <pine-field-description>Visible on your profile.</pine-field-description>
//!   <pine-field-error>Please enter your name.</pine-field-error>
//! </pine-field-root>
//! ```
//!
//! `invalid` is two-way bindable via `pp-model:invalid="is_bad"`
//! so author-side validators can drive the Error visibility from
//! outside; native HTML constraint validation also flips it via
//! the `invalid` event installed on the Control's inner input.

use crate::form::FORM;
use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, watch_scope_field};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlInputElement};

create_context!(ROOT: Handle<PineFieldRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFieldRoot.poco", role = "scope", display = "contents")]
// RFC 049 — a Field bundles a Label + Control + optional
// Description + optional Error message. `accepts` because
// the actual input element lives inside Control as author
// content, and authors commonly drop icons / layout wrappers.
#[slot(default, accepts = [
    PineFieldLabel,
    PineFieldControl,
    PineFieldDescription,
    PineFieldError,
])]
pub struct PineFieldRoot {
    /// Form field name. Mirrored onto the Root as a hook for
    /// styling / DOM queries; the underlying `<input>`'s own
    /// `name="…"` still drives submission.
    #[prop]
    pub name: String,
    /// Forwarded onto the Control's inner input and stamped as
    /// `data-disabled` on every Field part.
    #[prop]
    pub disabled: bool,
    /// External validity override. Two-way bindable via
    /// `pp-model:invalid="…"`. Gates the Error part and the
    /// Control's `aria-invalid` mirror.
    #[prop]
    pub invalid: bool,

    /// Id the Label's `for` / Control's id share. Generated in
    /// `on_setup` from the Root's scope id.
    pub control_id: String,
    /// Id the Description uses. Control wires it into
    /// `aria-describedby`.
    pub description_id: String,
    /// Id the Error uses. Control wires it into
    /// `aria-errormessage`.
    pub error_id: String,

    /// `true` once the control has been blurred at least once.
    pub touched: bool,
    /// `true` when the control's value is non-empty.
    pub filled: bool,
    /// `true` while the control holds focus.
    pub focused: bool,
    /// `true` once the value has changed from its initial.
    pub dirty: bool,
}

#[handlers]
impl PineFieldRoot {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.control_id = format!("pine-field-control-{}", scope.0);
            self.description_id = format!("pine-field-description-{}", scope.0);
            self.error_id = format!("pine-field-error-{}", scope.0);
        }
        ROOT.provide(this::<Self>());

        // Wire into an enclosing `<pine-form-root>`: read the
        // initial error state for our `name`, then subscribe so
        // later mutations of `form.errors` flow onto `self.invalid`
        // without the author touching `pp-model:invalid`.
        if let Some(form) = FORM.inject() {
            let name = self.name.clone();
            let initial = form.with(|f| f.errors.contains_key(&name));
            if initial {
                self.invalid = true;
            }
            let handle = this::<Self>();
            let name_for_watch = name;
            watch_scope_field::<HashMap<String, String>, _>(
                form.scope_id(),
                "errors",
                move |errors, _prev| {
                    let has = errors.contains_key(&name_for_watch);
                    handle.update(|f: &mut PineFieldRoot| f.invalid = has);
                },
            );
        }
    }
}

// ── Label ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFieldLabel.poco", role = "label")]
pub struct PineFieldLabel {
    #[observe(ROOT)]
    pub control_id: String,
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub invalid: bool,
    #[observe(ROOT)]
    pub touched: bool,
    #[observe(ROOT)]
    pub dirty: bool,
    #[observe(ROOT)]
    pub filled: bool,
    #[observe(ROOT)]
    pub focused: bool,
}

#[handlers]
impl PineFieldLabel {}

// ── Control ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineFieldControl.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — Control is a wrapper slot. Authors drop a native
// `<input>` or a Pine input primitive here; `accepts` keeps
// this loose while documenting `PineInput` as the canonical
// control.
#[slot(default, accepts = [crate::input::PineInput])]
pub struct PineFieldControl {}

#[handlers]
impl PineFieldControl {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let Some(wrap) = refs.get("slot") else { return };
        let Some(inner) = find_control(&wrap) else {
            return;
        };

        let (control_id, description_id, error_id, invalid, disabled) = root.with(|r| {
            (
                r.control_id.clone(),
                r.description_id.clone(),
                r.error_id.clone(),
                r.invalid,
                r.disabled,
            )
        });
        let _ = inner.set_attribute("id", &control_id);
        let _ = inner.set_attribute("aria-describedby", &format!("{description_id} {error_id}"));
        let _ = inner.set_attribute("aria-invalid", if invalid { "true" } else { "false" });
        let _ = inner.set_attribute("aria-errormessage", &error_id);
        if disabled {
            let _ = inner.set_attribute("disabled", "");
        }

        // Seed `filled` from whatever value the author authored
        // on the input. Deferred because `on_ready` holds an
        // immutable borrow on this scope's state — a sibling
        // `root.update` during that frame is a sibling scope but
        // the reactive flush it triggers can still reenter ours.
        let root_for_seed = root.clone();
        let inner_for_seed = inner.clone();
        pocopine::tick::next(move || {
            let filled = inner_for_seed
                .clone()
                .dyn_into::<HtmlInputElement>()
                .map(|i| !i.value().is_empty())
                .unwrap_or(false);
            root_for_seed.update(|r: &mut PineFieldRoot| r.filled = filled);
        });

        // Reactive: keep aria-invalid in sync with Root.invalid
        // so external validators (or the native `invalid` event
        // below) flow onto the DOM.
        let inner_for_invalid = inner.clone();
        watch_scope_field::<bool, _>(root.scope_id(), "invalid", move |&v, _| {
            let _ =
                inner_for_invalid.set_attribute("aria-invalid", if v { "true" } else { "false" });
        });
        // Reactive: forward Root.disabled onto the inner input.
        let inner_for_disabled = inner.clone();
        watch_scope_field::<bool, _>(root.scope_id(), "disabled", move |&v, _| {
            if v {
                let _ = inner_for_disabled.set_attribute("disabled", "");
            } else {
                let _ = inner_for_disabled.remove_attribute("disabled");
            }
        });

        // ── focus / blur / input / invalid listeners ─────────
        // Each `on!` registers a typed listener bound to the
        // current scope — when the Field's Control unmounts the
        // listener is removed automatically (no `forget` leak).
        let root_for_focus = root.clone();
        on!(inner, focus, |_e| {
            root_for_focus.update(|r| r.focused = true);
        });

        let root_for_blur = root.clone();
        on!(inner, blur, |_e| {
            root_for_blur.update(|r| {
                r.focused = false;
                r.touched = true;
            });
        });

        let root_for_input = root.clone();
        let inner_for_input = inner.clone();
        on!(inner, input, |_e| {
            let val = inner_for_input
                .clone()
                .dyn_into::<HtmlInputElement>()
                .map(|i| i.value())
                .unwrap_or_default();
            let filled = !val.is_empty();
            root_for_input.update(|r| {
                r.dirty = true;
                r.filled = filled;
            });
        });

        // Native constraint-validation failure (`required`,
        // `pattern`, `min` / `max`, etc.) → Root.invalid.
        on!(inner, invalid, |_e| {
            root.update(|r| r.invalid = true);
        });
    }
}

/// Locate the first native form control under the Control
/// wrapper — checked in the order `<input>`, `<textarea>`,
/// `<select>` so the common case (a plain text input) is fastest.
fn find_control(wrap: &Element) -> Option<Element> {
    for sel in ["input", "textarea", "select"] {
        if let Some(el) = wrap.query_selector(sel).ok().flatten() {
            return Some(el);
        }
    }
    None
}

// ── Description ───────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFieldDescription.poco", role = "text")]
pub struct PineFieldDescription {
    #[observe(ROOT)]
    pub description_id: String,
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub invalid: bool,
}

#[handlers]
impl PineFieldDescription {}

// ── Error ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFieldError.poco", role = "text")]
pub struct PineFieldError {
    #[observe(ROOT)]
    pub error_id: String,
    #[observe(ROOT)]
    pub invalid: bool,
}

#[handlers]
impl PineFieldError {}
