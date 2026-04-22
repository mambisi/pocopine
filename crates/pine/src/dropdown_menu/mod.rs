//! `<pine-dropdown-menu-*>` — compound menu primitive.
//!
//! Radix-style anatomy: state owned by `Root`, rendered via named
//! sub-parts (Trigger, Portal, Content, Item) that talk to Root via
//! RFC-027 `provide`/`inject`. Every sub-part has its own scope and
//! ARIA role; Root has no DOM of its own (pure state container).
//!
//! ```html
//! <pine-dropdown-menu-root>
//!   <pine-dropdown-menu-trigger>Actions ▾</pine-dropdown-menu-trigger>
//!   <pine-dropdown-menu-portal>
//!     <pine-dropdown-menu-content>
//!       <pine-dropdown-menu-group>
//!         <pine-dropdown-menu-label>Actions</pine-dropdown-menu-label>
//!         <pine-dropdown-menu-item @click="bump">Bump</pine-dropdown-menu-item>
//!         <pine-dropdown-menu-item disabled>Export</pine-dropdown-menu-item>
//!       </pine-dropdown-menu-group>
//!       <pine-dropdown-menu-separator></pine-dropdown-menu-separator>
//!       <pine-dropdown-menu-item @click="reset">Reset</pine-dropdown-menu-item>
//!     </pine-dropdown-menu-content>
//!   </pine-dropdown-menu-portal>
//! </pine-dropdown-menu-root>
//! ```
//!
//! Content auto-anchors to its Trigger via RFC-027 inject + the
//! `on_setup` lifecycle — no selector required.

use crate::compound;
use pocopine::prelude::*;
use pocopine::{current_scope_id, focus, inject, inject_key, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::Element;

const SLUG: &str = "dm";
const SUB_SLUG: &str = "dm-sub";

// Provide/inject key for the Root handle.
inject_key!(ROOT: Handle<PineDropdownMenuRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuRoot.poco", role = "scope")]
pub struct PineDropdownMenuRoot {
    /// Open state. Two-way bindable via `pp-model:open="current"`
    /// on the tag.
    #[prop] pub open: bool,
}

#[handlers]
impl PineDropdownMenuRoot {
    // Must be `on_setup` (pre-children-walk), not `on_mount`
    // (post-children-walk). Every descendant's `#[observe(ROOT)]`
    // field installs its observer during its OWN `on_setup`, which
    // fires while the Root's subtree is still being walked —
    // providing in `on_mount` runs AFTER all of that so every
    // descendant's observer never finds ROOT in the inject chain
    // and silently skips (Trigger/Portal/Content/etc. all see
    // `open` stuck at false). Same pattern every other Pine
    // compound uses; DropdownMenu was the odd one out.
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }

    pub fn open_menu(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuTrigger.poco", role = "interactive")]
pub struct PineDropdownMenuTrigger {
    /// Mirrored from Root.open so the template's `:aria-expanded`
    /// and `:data-state` bindings fire reactively.
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineDropdownMenuTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(&ROOT) else {
            return;
        };
        // Stamp the trigger's button with its root scope id.
        // Every Pine dropdown on the page gets a unique value so
        // multiple menus don't collide on the shared selector.
        // Content mirrors the same id into its own `anchor`
        // field in `on_setup`.
        if let Some(btn) = refs.get("trigger") {
            compound::stamp_trigger(&btn, root.scope_id(), SLUG);
        }
    }

    pub fn toggle(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDropdownMenuRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuPortal.poco", role = "scope")]
pub struct PineDropdownMenuPortal {
    /// Mirrored from Root.open so the template's `pp-if` fires the
    /// teleport when Root opens / closes.
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineDropdownMenuPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineDropdownMenuContent.poco", role = "list", transition = "slide-down")]
pub struct PineDropdownMenuContent {
    /// Computed in `on_setup` from the injected root scope id —
    /// a per-instance selector targeting this root's Trigger
    /// button. Authors never write it; Content resolves it
    /// automatically via context.
    pub anchor: String,
    /// Which side of the trigger the content sits on —
    /// `"top"` / `"bottom"` / `"left"` / `"right"`. Default
    /// `"bottom"`.
    #[prop] pub side: String,
    /// Cross-axis alignment — `"start"` / `"center"` / `"end"`.
    /// Default `"start"`.
    #[prop] pub align: String,
    /// Pixel offset from the trigger. Default `4`.
    #[prop] pub side_offset: f64,
}

impl Default for PineDropdownMenuContent {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            side: "bottom".into(),
            align: "start".into(),
            side_offset: 4.0,
        }
    }
}

#[handlers]
impl PineDropdownMenuContent {
    /// Runs before the template walks, so pp-anchor sees the
    /// computed selector on first bind. Uses the root's scope id
    /// so every menu instance on the page has its own anchor —
    /// matching the unique `data-pine-dm-trigger="N"` Trigger
    /// stamped in its `on_ready`.
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
        // Expose `side` to any nested Arrow.
        provide(&CONTENT_SIDE, self.side.clone());
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        // Auto-focus the first menuitem once the teleported clone
        // has committed. Items live in the slot which only
        // materialises after Portal flips `pp-if` on, so this is
        // the first point we can see them.
        let Some(menu) = refs.get("menu") else { return };
        init_roving_tabindex(&menu);
        focus::auto_focus_first(&menu);

        // Exempt our own trigger from `@click.outside`, so clicking
        // the trigger while the menu is open closes cleanly instead
        // of racing between outside-close (capture) and
        // trigger-toggle (bubble). See directives/on.rs for how
        // `data-pp-outside-exempt` is consumed.
        if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(&ROOT) {
            let _ = menu.set_attribute(
                "data-pp-outside-exempt",
                &compound::trigger_selector(root.scope_id(), SLUG),
            );
        }

        // Anchor the menu to the trigger programmatically so we
        // can honour the author's side/align/side_offset props
        // (the pp-anchor directive form parses modifiers
        // statically at bind time).
        if let Ok(floater) = menu.dyn_into::<web_sys::HtmlElement>() {
            if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(&ROOT) {
                compound::install_anchor_to_trigger(
                    &floater,
                    root.scope_id(),
                    SLUG,
                    &self.side,
                    &self.align,
                    self.side_offset,
                    true,
                );
            }
        }
    }

    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuItem.poco", role = "item")]
pub struct PineDropdownMenuItem {
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineDropdownMenuItem {
    /// Fires on the inner `<li>`'s click. Two-step dismiss:
    ///
    /// 1. Dispatch a cancelable `pp:select` CustomEvent
    ///    synchronously on the element. Authors can listen with
    ///    `@pp:select.prevent="…"` on the tag to veto the
    ///    auto-close — the menu stays open while their action
    ///    still runs via the native click bubble.
    /// 2. If no listener called `preventDefault()`, close the
    ///    menu via the injected root.
    ///
    /// Matches reka-ui's `DropdownMenuItem` select-emits-with-
    /// preventable semantic.
    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        let prevented = dispatch_pp_select();
        if prevented {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

/// Dispatch a cancelable `pp:select` event from the current
/// directive element via the substrate helper. Returns `true`
/// when a listener called `preventDefault` — caller should skip
/// its default action.
fn dispatch_pp_select() -> bool {
    emit_cancelable("pp:select", ())
}

// ── Sub / SubTrigger / SubContent ─────────────────────────────────

/// Nested submenu root. Independent `open` state provided under
/// a separate context key (`SUB`) — the outer menu's
/// `ROOT` flows through so SubContent can still reach it
/// for outer-dismiss semantics when needed.
///
/// v0 is click-to-open (no hover-intent timers). Escape in
/// SubContent closes just the sub, not the outer menu.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuSub.poco", role = "scope")]
pub struct PineDropdownMenuSub {
    pub open: bool,
}

inject_key!(SUB: Handle<PineDropdownMenuSub>);

#[handlers]
impl PineDropdownMenuSub {
    pub fn on_setup(&mut self) {
        provide(&SUB, this::<Self>());
    }

    pub fn close(&mut self) {
        self.open = false;
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuSubTrigger.poco", role = "item")]
pub struct PineDropdownMenuSubTrigger {
    #[observe(SUB)] pub open: bool,
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineDropdownMenuSubTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(sub) = inject::<Handle<PineDropdownMenuSub>>(&SUB) else { return };
        // Stamp so SubContent can auto-anchor.
        if let Some(btn) = refs.get("trigger") {
            compound::stamp_trigger(&btn, sub.scope_id(), SUB_SLUG);
        }
    }

    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        // Opening a sub-menu should NOT dismiss the parent
        // (reka keeps the parent open), so we veto the default
        // Item-select dismissal. Item.on_select still fires first
        // via native bubble if the author stacked handlers.
        if let Some(sub) = inject(&SUB) {
            sub.update(|s: &mut PineDropdownMenuSub| s.toggle());
        }
    }
}

#[derive(Serialize, Deserialize)]
#[component(template = "PineDropdownMenuSubContent.poco", role = "list", transition = "slide-down")]
pub struct PineDropdownMenuSubContent {
    pub open: bool,
    pub anchor: String,
    #[prop] pub side: String,
    #[prop] pub align: String,
    #[prop] pub side_offset: f64,
}

impl Default for PineDropdownMenuSubContent {
    fn default() -> Self {
        Self {
            open: false,
            anchor: String::new(),
            // Submenus conventionally pop out to the right.
            side: "right".into(),
            align: "start".into(),
            side_offset: 2.0,
        }
    }
}

#[handlers]
impl PineDropdownMenuSubContent {
    pub fn on_setup(&mut self) {
        if let Some(sub) = inject(&SUB) {
            self.anchor = compound::trigger_selector(sub.scope_id(), SUB_SLUG);
            self.open = sub.with(|s| s.open);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(sub) = inject(&SUB) else { return };
        let sub_scope = sub.scope_id();
        // Watch for the sub's open transitions so roving-focus +
        // auto-focus can run when the menu mounts. pp-anchor is
        // handled declaratively in the template; we only need to
        // forward `open` into self.
        watch_scope_field::<bool, _>(sub_scope, "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
            if is_open {
                focus_first_sub_item();
            }
        });
    }

    pub fn close(&mut self) {
        if let Some(sub) = inject(&SUB) {
            sub.update(|s: &mut PineDropdownMenuSub| s.close());
        }
    }
}

/// Give keyboard focus to the first enabled item in the sub
/// menu once it mounts. Runs via `tick::next` so pp-if has
/// actually cloned + walked the teleported subtree before we
/// query for the menu ref.
fn focus_first_sub_item() {
    pocopine::tick::next(|| {
        let Some(scope) = current_scope_id() else { return };
        let Some(menu) = refs::get_on(scope, "menu") else { return };
        init_roving_tabindex(&menu);
        focus::auto_focus_first(&menu);
    });
}

// ── Arrow ─────────────────────────────────────────────────────────

/// Decorative arrow that points at the trigger. Inherits its
/// orientation from Content's `side` prop via inject + mirror.
///
/// v0 limitation: the arrow reflects the *configured* side, not
/// the resolved side after a collision flip. For tooltips that
/// never flip (`flip=false`) or menus authored with known space,
/// this is correct. Adding runtime-flip awareness would require
/// `pp-anchor::reposition` to expose the resolved side through
/// a side-table — saved for a follow-up.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuArrow.poco", role = "visual")]
pub struct PineDropdownMenuArrow {
    pub side: String,
}

// Provide/inject key for Content's `side` — Arrow subscribes to
// this to render its own `data-side`. Content writes its value
// in `on_setup`.
inject_key!(CONTENT_SIDE: String);

#[handlers]
impl PineDropdownMenuArrow {
    pub fn on_setup(&mut self) {
        if let Some(side) = inject(&CONTENT_SIDE) {
            self.side = side;
        }
    }
}

// ── Separator ─────────────────────────────────────────────────────

/// Visual divider between groups of menu items. No state, no
/// focus, no interaction — pure `role="separator"` +
/// `aria-orientation`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuSeparator.poco", role = "item")]
pub struct PineDropdownMenuSeparator {}

#[handlers]
impl PineDropdownMenuSeparator {}

// ── Group ─────────────────────────────────────────────────────────

/// ARIA group wrapper. Its `on_setup` mints a `label_id` from its
/// own scope id (unique per instance) and provides it to any
/// nested Label so their ids match up for `aria-labelledby`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuGroup.poco", role = "panel")]
pub struct PineDropdownMenuGroup {
    /// Computed — id of the Label inside this group, for
    /// `aria-labelledby` on the group's root. Populated in
    /// `on_setup`; authors never set it.
    pub label_id: String,
}

// Provide/inject key for a Group's label id. Only meaningful
// inside a Group's subtree.
inject_key!(GROUP_LABEL: String);

#[handlers]
impl PineDropdownMenuGroup {
    pub fn on_setup(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        let label_id = format!("pine-dm-group-label-{}", scope.0);
        self.label_id = label_id.clone();
        provide(&GROUP_LABEL, label_id);
    }
}

// ── CheckboxItem ──────────────────────────────────────────────────

/// Toggleable menu item. `role="menuitemcheckbox"`, tri-state via
/// the inherited PineCheckbox state machine
/// (`"checked"`/`"unchecked"`/`"indeterminate"`), two-way
/// bindable through `pp-model:state`. Emits the same cancelable
/// `pp:select` as Item so the menu can be kept open.
///
/// Inside a CheckboxItem, a `<pine-dropdown-menu-item-indicator>`
/// renders its slot only when `checked != "unchecked"` (matches
/// reka-ui semantics).
#[derive(Serialize, Deserialize)]
#[component(template = "PineDropdownMenuCheckboxItem.poco", role = "item")]
pub struct PineDropdownMenuCheckboxItem {
    #[prop] pub state: String,
    #[prop] pub disabled: bool,
    /// Computed mirror of `state != "unchecked"` for
    /// ItemIndicator's pp-if. Kept as a bool so
    /// `watch_scope_field::<bool>` in ItemIndicator stays simple.
    pub checked: bool,
}

impl Default for PineDropdownMenuCheckboxItem {
    fn default() -> Self {
        Self {
            state: "unchecked".into(),
            disabled: false,
            checked: false,
        }
    }
}

// Provide/inject key for a Checkbox or Radio item's scope id —
// an ItemIndicator's subscription point. The value is the
// owner's `ScopeId`; ItemIndicator's `on_ready` uses it with
// `watch_scope_field` to mirror `checked`.
inject_key!(CHECKED_OWNER: ScopeId);

#[handlers]
impl PineDropdownMenuCheckboxItem {
    pub fn on_setup(&mut self) {
        // Derive the initial `checked` bool from `state` so
        // ItemIndicator's pp-if sees the right value on first
        // bind (before the `#[watch(state)]` below fires).
        self.checked = self.state != "unchecked";
        // Provide in on_setup, NOT on_mount: on_setup fires
        // before the template's children walk, so nested
        // ItemIndicator(s) can inject during their own
        // on_setup. on_mount fires post-children which would be
        // too late.
        if let Some(scope) = current_scope_id() {
            provide(&CHECKED_OWNER, scope);
        }
    }

    /// Mirror `state` → `checked` reactively so any nested
    /// ItemIndicator's pp-if re-evaluates on state changes.
    #[watch(state)]
    fn on_state_change(&mut self, state: String, _prev: Option<String>) {
        self.checked = state != "unchecked";
    }

    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        // Cycle: unchecked → checked → unchecked; indeterminate → checked.
        self.state = match self.state.as_str() {
            "checked" => "unchecked".into(),
            _ => "checked".into(),
        };
        emit("pp:update:model", self.state.clone());

        let prevented = dispatch_pp_select();
        if prevented {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

// ── ItemIndicator ─────────────────────────────────────────────────

/// Renders its slot only when the enclosing CheckboxItem /
/// RadioItem is currently checked. Injects the owner's scope id
/// and mirrors its `checked` field through
/// `watch_scope_field`, so any state change in the parent item
/// reactively shows/hides this indicator.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuItemIndicator.poco", role = "visual")]
pub struct PineDropdownMenuItemIndicator {
    pub checked: bool,
}

#[handlers]
impl PineDropdownMenuItemIndicator {
    pub fn on_setup(&mut self) {
        // Read the parent item's initial `checked` synchronously
        // so the first pp-show evaluation sees the right value.
        if let Some(owner) = inject(&CHECKED_OWNER) {
            if let Some(scope) = Scope::find(owner) {
                let v = scope.state.borrow().get("checked");
                self.checked = v.as_bool().unwrap_or(false);
            }
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(owner) = inject(&CHECKED_OWNER) else { return };
        watch_scope_field::<bool, _>(owner, "checked", move |&c, _| {
            handle.update(|s| s.checked = c);
        });
    }
}

// ── RadioGroup ────────────────────────────────────────────────────

/// Exclusive-selection container for `PineDropdownMenuRadioItem`s.
/// Owns a shared `value: String`; each RadioItem is "checked"
/// when its own `value` equals the group's. `pp-model:value`
/// flows changes both ways via `pp:update:model` bubbling up
/// from RadioItem clicks.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuRadioGroup.poco", role = "panel")]
pub struct PineDropdownMenuRadioGroup {
    #[prop] pub value: String,
}

// Provide/inject key for a RadioGroup's scope id. RadioItems
// inside use it to read the group's `value` (for their own
// `checked` mirror) and write to it on click.
inject_key!(RADIO_GROUP: ScopeId);

#[handlers]
impl PineDropdownMenuRadioGroup {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            provide(&RADIO_GROUP, scope);
        }
    }
}

// ── RadioItem ─────────────────────────────────────────────────────

/// Radio-selection menu item. `role="menuitemradio"`. Its
/// `checked` bool mirrors `group.value == self.value`, updated
/// reactively via `watch_scope_field` on the injected group.
/// Also provides `CHECKED_OWNER` so nested ItemIndicators
/// work identically to CheckboxItem.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuRadioItem.poco", role = "item")]
pub struct PineDropdownMenuRadioItem {
    /// Author-provided — the value this item represents.
    #[prop] pub value: String,
    /// Mirrored from the group. Only used to derive `checked`.
    pub group_value: String,
    /// Computed: `group_value == value`. Drives aria-checked +
    /// ItemIndicator visibility.
    pub checked: bool,
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineDropdownMenuRadioItem {
    pub fn on_setup(&mut self) {
        // Seed initial group_value + checked from the group,
        // and provide to ItemIndicator.
        if let Some(group) = inject(&RADIO_GROUP) {
            if let Some(scope) = Scope::find(group) {
                let v = scope.state.borrow().get("value");
                self.group_value = v.as_string().unwrap_or_default();
                self.checked = self.group_value == self.value;
            }
        }
        if let Some(scope) = current_scope_id() {
            provide(&CHECKED_OWNER, scope);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(group) = inject(&RADIO_GROUP) else { return };
        watch_scope_field::<String, _>(group, "value", move |new, _| {
            let new_v = new.clone();
            handle.update(|s| {
                s.group_value = new_v.clone();
                s.checked = s.group_value == s.value;
            });
        });
    }

    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        // Write the new value into the group; the group's
        // pp-model-bound parent picks this up via the
        // pp:update:model event we emit next.
        if let Some(group) = inject(&RADIO_GROUP) {
            if let Some(scope) = Scope::find(group) {
                let new_value = self.value.clone();
                let handle = scope
                    .typed::<PineDropdownMenuRadioGroup>()
                    .map(|rc| Handle::new(rc, group));
                if let Some(h) = handle {
                    h.update(|g: &mut PineDropdownMenuRadioGroup| {
                        g.value = new_value;
                    });
                }
            }
        }
        emit("pp:update:model", self.value.clone());

        let prevented = dispatch_pp_select();
        if prevented {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

// ── Label ─────────────────────────────────────────────────────────

/// Labelled heading for a Group. Injects the group's label id and
/// renders it as the element's `id` so the enclosing Group's
/// `aria-labelledby` resolves. Does not render a `role` — it's
/// styling-only (matches reka-ui / Radix).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuLabel.poco", role = "panel")]
pub struct PineDropdownMenuLabel {
    pub label_id: String,
}

#[handlers]
impl PineDropdownMenuLabel {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject(&GROUP_LABEL) {
            self.label_id = id;
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────

/// Set `tabindex=-1` on every menuitem and promote the first
/// non-disabled one to `tabindex=0` — the starting cursor for
/// `pp-roving`. Runs after the slot materialises so the items
/// are in the DOM.
fn init_roving_tabindex(menu: &Element) {
    let Ok(items) = menu.query_selector_all(
        "[role=\"menuitem\"], [role=\"menuitemradio\"], [role=\"menuitemcheckbox\"]",
    ) else {
        return;
    };
    let mut first_enabled: Option<Element> = None;
    for i in 0..items.length() {
        let Some(node) = items.item(i) else { continue };
        let Ok(el) = node.dyn_into::<Element>() else { continue };
        let _ = el.set_attribute("tabindex", "-1");
        let disabled = el.get_attribute("aria-disabled").as_deref() == Some("true")
            || el.has_attribute("disabled");
        if first_enabled.is_none() && !disabled {
            first_enabled = Some(el.clone());
        }
    }
    if let Some(el) = first_enabled {
        let _ = el.set_attribute("tabindex", "0");
    }
}
