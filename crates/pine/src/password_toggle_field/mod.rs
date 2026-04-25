//! `<pine-password-toggle-field-*>` — password input with an
//! integrated show/hide button (compound).
//!
//! Mirrors Radix `<PasswordToggleField>`. Three parts:
//!
//! - **Root** (`pine-password-toggle-field-root`) — holds
//!   `visible: bool` (two-way bindable via
//!   `pp-model:visible="my_bool"`). Provides its handle to
//!   descendants.
//! - **Input** (`pine-password-toggle-field-input`) — `pp-as`
//!   friendly; merges onto a user-provided `<input>` and binds
//!   its `type` attribute to `"text"` when `visible`, else
//!   `"password"`. Authors write the attrs they care about
//!   (`name`, `placeholder`, `required`, …) on the inner
//!   `<input>` and the Input component binds only the `type`.
//! - **Toggle** (`pine-password-toggle-field-toggle`) — button
//!   that flips Root.visible. `aria-pressed` mirrors the state
//!   so screen readers announce "pressed" when the password is
//!   shown.
//!
//! ```html
//! <pine-password-toggle-field-root>
//!   <pine-password-toggle-field-input pp-as>
//!     <input name="pwd" placeholder="Password" required>
//!   </pine-password-toggle-field-input>
//!   <pine-password-toggle-field-toggle>
//!     <span pp-show="visible">🙈</span>
//!     <span pp-show="!visible">👀</span>
//!   </pine-password-toggle-field-toggle>
//! </pine-password-toggle-field-root>
//! ```

use pocopine::prelude::*;
use pocopine::{create_context, watch_scope_field_scoped};
use serde::{Deserialize, Serialize};

create_context!(ROOT: Handle<PinePasswordToggleFieldRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePasswordToggleFieldRoot.poco", role = "panel")]
// RFC 049 — this compound pairs an Input (the password field)
// with a Toggle (the show/hide button). `only` rejects stray
// markup that would confuse the visibility-swap behaviour.
#[slot(default, only = [PinePasswordToggleFieldInput, PinePasswordToggleFieldToggle])]
pub struct PinePasswordToggleFieldRoot {
    /// Whether the password is currently shown. Two-way
    /// bindable via `pp-model:visible`.
    #[model]
    pub visible: bool,
}

#[handlers]
impl PinePasswordToggleFieldRoot {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }
}

// ── Input ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePasswordToggleFieldInput.poco", role = "scope")]
pub struct PinePasswordToggleFieldInput {}

#[handlers]
impl PinePasswordToggleFieldInput {
    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = ROOT.inject() else { return };
        let Some(wrap) = refs.get("slot") else { return };
        let Some(inp) = wrap.query_selector("input").ok().flatten() else {
            return;
        };
        let initial = root.with(|r| r.visible);
        let _ = inp.set_attribute("type", if initial { "text" } else { "password" });
        let inp_cap = inp.clone();
        watch_scope_field_scoped::<bool, _>(root.scope_id(), "visible", move |&v, _| {
            let _ = inp_cap.set_attribute("type", if v { "text" } else { "password" });
        });
    }
}

// ── Toggle ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePasswordToggleFieldToggle.poco", role = "interactive")]
pub struct PinePasswordToggleFieldToggle {
    /// Mirrored from Root for `:aria-pressed` + `:data-state`.
    #[observe(ROOT)]
    pub visible: bool,
}

#[handlers]
impl PinePasswordToggleFieldToggle {
    pub fn click(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PinePasswordToggleFieldRoot| r.toggle());
        }
    }
}
