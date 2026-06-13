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

use crate::{compound, overlay};
use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, focus, refs, watch_scope_field_scoped, ScopeId};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::Element;

const SLUG: &str = "dm";
const SUB_SLUG: &str = "dm-sub";
const SUB_CONTENT_ATTR: &str = "data-pine-dm-sub-content";
const SUB_CONTENT_ROOT_ATTR: &str = "data-pine-dm-sub-content-root";
const MENU_ITEM_SELECTOR: &str =
    "[role=\"menuitem\"], [role=\"menuitemradio\"], [role=\"menuitemcheckbox\"]";

fn sub_content_selector(root_scope: ScopeId) -> String {
    format!("[{SUB_CONTENT_ROOT_ATTR}=\"{}\"]", root_scope.0)
}

fn outside_exempt_selector(root_scope: ScopeId) -> String {
    format!(
        "{}, {}",
        compound::trigger_selector(root_scope, SLUG),
        sub_content_selector(root_scope)
    )
}

// Provide/inject key for the Root handle.
create_context!(ROOT: Handle<PineDropdownMenuRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineDropdownMenuRoot.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — a DropdownMenu's Root holds exactly the Trigger plus
// a Portal (the menu itself teleports). No bare Content at root
// level, no unrelated wrappers.
#[slot(default, only = [PineDropdownMenuTrigger, PineDropdownMenuPortal])]
pub struct PineDropdownMenuRoot {
    /// Open state. Two-way bindable via `pp-model:open="current"`
    /// on the tag.
    #[model]
    pub open: bool,
    pub closing: bool,
    pub open_submenus: u32,
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
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
    }

    pub fn open_menu(&mut self) {
        self.open = true;
        self.closing = false;
        self.open_submenus = 0;
    }

    pub fn close(&mut self) {
        if !self.open {
            self.closing = false;
            self.open_submenus = 0;
            return;
        }

        if self.open_submenus > 0 && !self.closing {
            self.closing = true;
            let root = this::<Self>();
            pocopine::tick::next_frame(move || {
                root.update(|r: &mut PineDropdownMenuRoot| {
                    if r.closing {
                        r.open = false;
                        r.closing = false;
                        r.open_submenus = 0;
                    }
                });
            });
            return;
        }

        self.open = false;
        self.closing = false;
        self.open_submenus = 0;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open_menu();
        }
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuTrigger.poco", role = "interactive")]
pub struct PineDropdownMenuTrigger {
    /// Mirrored from Root.open so the template's `:aria-expanded`
    /// and `:data-state` bindings fire reactively.
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PineDropdownMenuTrigger {
    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = ROOT.inject() else {
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
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineDropdownMenuRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineDropdownMenuPortal.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — Portal is the teleport wrapper; exactly one Content
// is expected inside. Any other element drops at runtime because
// the Content owns the menu surface and its ARIA / keyboard
// bindings.
#[slot(default, only = [PineDropdownMenuContent])]
pub struct PineDropdownMenuPortal {
    /// Mirrored from Root.open so the template's `pp-if` fires the
    /// teleport when Root opens / closes.
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PineDropdownMenuPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDropdownMenuContent.poco",
    role = "list",
    transition = "slide-down"
)]
// RFC 049 — the menu surface accepts the semantically-valid menu
// children only. `pp-roving` + `role="menu"` depend on these
// contracts: wrapping items in a <div> moves them out of the
// roving tab chain; unrelated components break keyboard navigation
// and ARIA announcements.
#[slot(default, only = [
    PineDropdownMenuItem,
    PineDropdownMenuCheckboxItem,
    PineDropdownMenuSeparator,
    PineDropdownMenuLabel,
    PineDropdownMenuGroup,
    PineDropdownMenuRadioGroup,
    PineDropdownMenuSub,
    PineDropdownMenuArrow,
])]
pub struct PineDropdownMenuContent {
    /// Computed in `on_setup` from the injected root scope id —
    /// a per-instance selector targeting this root's Trigger
    /// button. Authors never write it; Content resolves it
    /// automatically via context.
    pub anchor: String,
    /// Which side of the trigger the content sits on —
    /// `"top"` / `"bottom"` / `"left"` / `"right"`. Default
    /// `"bottom"`.
    #[prop]
    pub side: String,
    /// Cross-axis alignment — `"start"` / `"center"` / `"end"`.
    /// Default `"start"`.
    #[prop]
    pub align: String,
    /// Pixel offset from the trigger. Default `4`.
    #[prop]
    pub side_offset: f64,
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
    fn on_setup(&mut self) {
        if let Some(root) = ROOT.inject() {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
        // Expose `side` to any nested Arrow.
        CONTENT_SIDE.provide(self.side.clone());
    }

    fn on_ready(&self, refs: pocopine::Refs) {
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
        if let Some(root) = ROOT.inject() {
            let _ = menu.set_attribute(
                "data-pp-outside-exempt",
                &outside_exempt_selector(root.scope_id()),
            );
        }

        // Anchor the menu to the trigger programmatically so we
        // can honour the author's side/align/side_offset props
        // (the pp-anchor directive form parses modifiers
        // statically at bind time).
        if let Ok(floater) = menu.dyn_into::<web_sys::HtmlElement>()
            && let Some(root) = ROOT.inject() {
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

    pub fn close(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }

    pub fn on_pointer_down_outside(&mut self) {
        if overlay::dispatch_pointer_down_outside() {
            return;
        }
        self.close();
    }

    pub fn on_interact_outside(&mut self) {
        if overlay::dispatch_interact_outside() {
            return;
        }
        self.close();
    }

    pub fn isolate_interaction(&mut self) {}
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuItem.poco", role = "item")]
pub struct PineDropdownMenuItem {
    #[prop]
    pub disabled: bool,
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
        if let Some(root) = ROOT.inject() {
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
#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDropdownMenuSub.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — a Sub is always exactly SubTrigger + SubContent.
// SubContent renders teleported, but the Sub wrapper owns both
// halves in source order.
#[slot(default, only = [PineDropdownMenuSubTrigger, PineDropdownMenuSubContent])]
pub struct PineDropdownMenuSub {
    pub open: bool,
    pub content_side: String,
    pub content_align: String,
    pub content_side_offset: f64,
}

impl Default for PineDropdownMenuSub {
    fn default() -> Self {
        Self {
            open: false,
            content_side: "right".into(),
            content_align: "start".into(),
            content_side_offset: 2.0,
        }
    }
}

create_context!(SUB: Handle<PineDropdownMenuSub>);

#[handlers]
impl PineDropdownMenuSub {
    fn on_setup(&mut self) {
        SUB.provide(this::<Self>());
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = ROOT.inject() else { return };
        watch_scope_field_scoped::<bool, _>(root.scope_id(), "closing", move |&closing, _| {
            if closing {
                handle.update(|s: &mut PineDropdownMenuSub| s.set_open(false));
            }
        });
    }

    fn set_open(&mut self, next: bool) {
        if self.open == next {
            return;
        }
        self.open = next;
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineDropdownMenuRoot| {
                if next {
                    r.open_submenus = r.open_submenus.saturating_add(1);
                } else {
                    r.open_submenus = r.open_submenus.saturating_sub(1);
                }
            });
        }
    }

    pub fn close(&mut self) {
        self.set_open(false);
    }

    pub fn toggle(&mut self) {
        self.set_open(!self.open);
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuSubTrigger.poco", role = "item")]
pub struct PineDropdownMenuSubTrigger {
    #[observe(SUB)]
    pub open: bool,
    #[prop]
    pub disabled: bool,
}

#[handlers]
impl PineDropdownMenuSubTrigger {
    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(sub) = SUB.inject() else {
            return;
        };
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
        if let Some(sub) = SUB.inject() {
            let is_open = sub.update(|s: &mut PineDropdownMenuSub| {
                s.set_open(!s.open);
                s.open
            });
            if is_open {
                prepare_open_sub_content_from_sub(&sub);
            }
        }
    }

    pub fn open(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(sub) = SUB.inject() {
            sub.update(|s: &mut PineDropdownMenuSub| s.set_open(true));
            prepare_open_sub_content_from_sub(&sub);
        }
    }
}

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDropdownMenuSubContent.poco",
    role = "list",
    transition = "slide-down"
)]
// RFC 049 — SubContent hosts the same menu-items surface as
// Content. Same accepts list.
#[slot(default, only = [
    PineDropdownMenuItem,
    PineDropdownMenuCheckboxItem,
    PineDropdownMenuSeparator,
    PineDropdownMenuLabel,
    PineDropdownMenuGroup,
    PineDropdownMenuRadioGroup,
    PineDropdownMenuSub,
    PineDropdownMenuArrow,
])]
pub struct PineDropdownMenuSubContent {
    pub open: bool,
    pub anchor: String,
    pub sub_id: String,
    pub root_id: String,
    #[prop]
    pub side: String,
    #[prop]
    pub align: String,
    #[prop]
    pub side_offset: f64,
}

impl Default for PineDropdownMenuSubContent {
    fn default() -> Self {
        Self {
            open: false,
            anchor: String::new(),
            sub_id: String::new(),
            root_id: String::new(),
            // Submenus conventionally pop out to the right.
            side: "right".into(),
            align: "start".into(),
            side_offset: 2.0,
        }
    }
}

#[handlers]
impl PineDropdownMenuSubContent {
    fn on_setup(&mut self) {
        if let Some(sub) = SUB.inject() {
            let sub_scope = sub.scope_id();
            self.anchor = compound::trigger_selector(sub_scope, SUB_SLUG);
            self.sub_id = sub_scope.0.to_string();
            self.open = sub.with(|s| s.open);
            let side = self.side.clone();
            let align = self.align.clone();
            let side_offset = self.side_offset;
            sub.update(|s: &mut PineDropdownMenuSub| {
                s.content_side = side;
                s.content_align = align;
                s.content_side_offset = side_offset;
            });
        }
        if let Some(root) = ROOT.inject() {
            self.root_id = root.scope_id().0.to_string();
        }
        CONTENT_SIDE.provide(self.side.clone());
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(sub) = SUB.inject() else { return };
        let sub_scope = sub.scope_id();
        let content_scope = handle.scope_id();
        let root_scope = ROOT.inject().map(|root| root.scope_id());
        let side = self.side.clone();
        let align = self.align.clone();
        let side_offset = self.side_offset;
        // Watch for the sub's open transitions so roving-focus +
        // auto-focus + programmatic anchoring can run after the
        // pp-if subtree mounts.
        watch_scope_field_scoped::<bool, _>(sub_scope, "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
            if is_open {
                prepare_open_sub_content(
                    content_scope,
                    root_scope,
                    sub_scope,
                    side.clone(),
                    align.clone(),
                    side_offset,
                );
            }
        });

        if sub.with(|s| s.open) {
            prepare_open_sub_content(
                content_scope,
                root_scope,
                sub_scope,
                self.side.clone(),
                self.align.clone(),
                self.side_offset,
            );
        }
    }

    pub fn close(&mut self) {
        if let Some(sub) = SUB.inject() {
            sub.update(|s: &mut PineDropdownMenuSub| s.close());
        }
    }

    pub fn isolate_interaction(&mut self) {}
}

/// Finish submenu setup once its pp-if subtree has cloned and
/// walked: stamp it into the parent dismiss boundary, anchor it
/// to the sub-trigger, and move focus into the first enabled item.
fn prepare_open_sub_content_from_sub(sub: &Handle<PineDropdownMenuSub>) {
    let sub_scope = sub.scope_id();
    let root_scope = ROOT.inject().map(|root| root.scope_id());
    let (side, align, side_offset) = sub.with(|s| {
        (
            s.content_side.clone(),
            s.content_align.clone(),
            s.content_side_offset,
        )
    });
    prepare_open_sub_content(sub_scope, root_scope, sub_scope, side, align, side_offset);
}

fn prepare_open_sub_content(
    content_scope: ScopeId,
    root_scope: Option<ScopeId>,
    sub_scope: ScopeId,
    side: String,
    align: String,
    side_offset: f64,
) {
    prepare_open_sub_content_attempt(
        content_scope,
        root_scope,
        sub_scope,
        side,
        align,
        side_offset,
        0,
    );
}

fn prepare_open_sub_content_attempt(
    content_scope: ScopeId,
    root_scope: Option<ScopeId>,
    sub_scope: ScopeId,
    side: String,
    align: String,
    side_offset: f64,
    attempt: u8,
) {
    pocopine::tick::next(move || {
        let ready_menu = resolve_open_sub_content_menu(content_scope, sub_scope).and_then(|menu| {
            let has_items = menu
                .query_selector(MENU_ITEM_SELECTOR)
                .ok()
                .flatten()
                .is_some();
            if has_items || attempt >= 5 {
                Some(menu)
            } else {
                None
            }
        });

        let Some(menu) = ready_menu else {
            if attempt < 5 {
                prepare_open_sub_content_attempt(
                    content_scope,
                    root_scope,
                    sub_scope,
                    side,
                    align,
                    side_offset,
                    attempt + 1,
                );
            }
            return;
        };

        if let Some(root_scope) = root_scope {
            let _ = menu.set_attribute(SUB_CONTENT_ROOT_ATTR, &root_scope.0.to_string());
        }

        init_roving_tabindex(&menu);
        focus::auto_focus_first(&menu);

        if let Ok(floater) = menu.clone().dyn_into::<web_sys::HtmlElement>() {
            compound::install_anchor_to_trigger(
                &floater,
                sub_scope,
                SUB_SLUG,
                &side,
                &align,
                side_offset,
                true,
            );
        }
    });
}

fn resolve_open_sub_content_menu(content_scope: ScopeId, sub_scope: ScopeId) -> Option<Element> {
    let selector = format!("[{SUB_CONTENT_ATTR}=\"{}\"]", sub_scope.0);
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(&selector).ok().flatten())
        .or_else(|| refs::get_on(content_scope, "menu"))
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
create_context!(CONTENT_SIDE: String);

#[handlers]
impl PineDropdownMenuArrow {
    fn on_setup(&mut self) {
        if let Some(side) = CONTENT_SIDE.inject() {
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
// RFC 049 — Groups hold a Label + items. Authors sometimes stack
// CheckboxItems or mix Separators inside one group; all permitted.
#[slot(default, only = [
    PineDropdownMenuLabel,
    PineDropdownMenuItem,
    PineDropdownMenuCheckboxItem,
    PineDropdownMenuSeparator,
])]
pub struct PineDropdownMenuGroup {
    /// Computed — id of the Label inside this group, for
    /// `aria-labelledby` on the group's root. Populated in
    /// `on_setup`; authors never set it.
    pub label_id: String,
}

// Provide/inject key for a Group's label id. Only meaningful
// inside a Group's subtree.
create_context!(GROUP_LABEL: String);

#[handlers]
impl PineDropdownMenuGroup {
    fn on_setup(&mut self) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let label_id = format!("pine-dm-group-label-{}", scope.0);
        self.label_id = label_id.clone();
        GROUP_LABEL.provide(label_id);
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
// RFC 049 — CheckboxItem commonly holds an ItemIndicator
// (the ✓ glyph) alongside the label. `accepts` (blanket) so
// authors can drop icons, layout wrappers, Indicator, or
// plain text.
#[slot(default, accepts = [PineDropdownMenuItemIndicator])]
pub struct PineDropdownMenuCheckboxItem {
    #[model]
    pub state: String,
    #[prop]
    pub disabled: bool,
    /// Computed mirror of `state != "unchecked"` for
    /// ItemIndicator's pp-if. Kept as a bool so
    /// `watch_scope_field_scoped::<bool>` in ItemIndicator stays simple.
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
// `watch_scope_field_scoped` to mirror `checked`.
create_context!(CHECKED_OWNER: ScopeId);

#[handlers]
impl PineDropdownMenuCheckboxItem {
    fn on_setup(&mut self) {
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
            CHECKED_OWNER.provide(scope);
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

        let prevented = dispatch_pp_select();
        if prevented {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

// ── ItemIndicator ─────────────────────────────────────────────────

/// Renders its slot only when the enclosing CheckboxItem /
/// RadioItem is currently checked. Injects the owner's scope id
/// and mirrors its `checked` field through
/// `watch_scope_field_scoped`, so any state change in the parent item
/// reactively shows/hides this indicator.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuItemIndicator.poco", role = "visual")]
pub struct PineDropdownMenuItemIndicator {
    pub checked: bool,
}

#[handlers]
impl PineDropdownMenuItemIndicator {
    fn on_setup(&mut self) {
        // Read the parent item's initial `checked` synchronously
        // so the first pp-show evaluation sees the right value.
        if let Some(owner) = CHECKED_OWNER.inject()
            && let Some(scope) = Scope::find(owner) {
                let v = scope.state.borrow().get("checked");
                self.checked = v.as_bool().unwrap_or(false);
            }
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(owner) = CHECKED_OWNER.inject() else {
            return;
        };
        watch_scope_field_scoped::<bool, _>(owner, "checked", move |&c, _| {
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
#[component(
    template = "PineDropdownMenuRadioGroup.poco",
    role = "panel",
    display = "contents"
)]
// RFC 049 — RadioGroups hold RadioItems + an optional Label +
// Separators. Regular Items are intentionally NOT accepted
// (the whole group is exclusive-choice; a non-radio item would
// break the selection semantic).
#[slot(default, only = [
    PineDropdownMenuRadioItem,
    PineDropdownMenuLabel,
    PineDropdownMenuSeparator,
])]
pub struct PineDropdownMenuRadioGroup {
    #[model]
    pub value: String,
}

// Provide/inject key for a RadioGroup's scope id. RadioItems
// inside use it to read the group's `value` (for their own
// `checked` mirror) and write to it on click.
create_context!(RADIO_GROUP: ScopeId);

#[handlers]
impl PineDropdownMenuRadioGroup {
    fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            RADIO_GROUP.provide(scope);
        }
    }
}

// ── RadioItem ─────────────────────────────────────────────────────

/// Radio-selection menu item. `role="menuitemradio"`. Its
/// `checked` bool mirrors `group.value == self.value`, updated
/// reactively via `watch_scope_field_scoped` on the injected group.
/// Also provides `CHECKED_OWNER` so nested ItemIndicators
/// work identically to CheckboxItem.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuRadioItem.poco", role = "item")]
// RFC 049 — same pattern as CheckboxItem: author content
// with an optional Indicator.
#[slot(default, accepts = [PineDropdownMenuItemIndicator])]
pub struct PineDropdownMenuRadioItem {
    /// Author-provided — the value this item represents.
    #[prop]
    pub value: String,
    /// Mirrored from the group. Only used to derive `checked`.
    pub group_value: String,
    /// Computed: `group_value == value`. Drives aria-checked +
    /// ItemIndicator visibility.
    pub checked: bool,
    #[prop]
    pub disabled: bool,
}

#[handlers]
impl PineDropdownMenuRadioItem {
    fn on_setup(&mut self) {
        // Seed initial group_value + checked from the group,
        // and provide to ItemIndicator.
        if let Some(group) = RADIO_GROUP.inject()
            && let Some(scope) = Scope::find(group) {
                let v = scope.state.borrow().get("value");
                self.group_value = v.as_string().unwrap_or_default();
                self.checked = self.group_value == self.value;
            }
        if let Some(scope) = current_scope_id() {
            CHECKED_OWNER.provide(scope);
        }
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(group) = RADIO_GROUP.inject() else {
            return;
        };
        watch_scope_field_scoped::<String, _>(group, "value", move |new, _| {
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
        // Write the new value into the group.
        if let Some(group) = RADIO_GROUP.inject()
            && let Some(scope) = Scope::find(group) {
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

        let prevented = dispatch_pp_select();
        if prevented {
            return;
        }
        if let Some(root) = ROOT.inject() {
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
    fn on_setup(&mut self) {
        if let Some(id) = GROUP_LABEL.inject() {
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
    let Ok(items) = menu.query_selector_all(MENU_ITEM_SELECTOR) else {
        return;
    };
    let mut first_enabled: Option<Element> = None;
    for i in 0..items.length() {
        let Some(node) = items.item(i) else { continue };
        let Ok(el) = node.dyn_into::<Element>() else {
            continue;
        };
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
