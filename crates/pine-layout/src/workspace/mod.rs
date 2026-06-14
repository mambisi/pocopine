//! `<pine-workspace-*>` — content-heavy multi-region workspace shell
//! (RFC-105).
//!
//! A separate component family from [`crate::app_shell`]: the workspace
//! frame adds the *trailing* edge (a resizable detail Aside, outer
//! panels, a bottom panel) and a resizable, rail-collapsing sidebar, on top
//! of the same headless `data-*` + breakpoint contract.
//!
//! **Headless** — every region emits a class + `data-*` state; the one
//! dynamic dimension CSS can't express statically (a resizable width/height)
//! is published as a `--pine-*-size` custom property the author's grid
//! consumes. Author CSS does the layout:
//!
//! ```css
//! .pine-workspace { display: grid; grid-template-columns: auto 1fr auto; }
//! .pine-workspace-sidebar { width: var(--pine-sidebar-size, 260px); }
//! .pine-workspace-sidebar[data-state="collapsed"] { /* rail look */ }
//! ```

pub(crate) mod resize;

use crate::workspace::resize::Axis;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

create_context!(WORKSPACE: Handle<PineWorkspace>);

// ── Root ──────────────────────────────────────────────────────────

/// The workspace frame. Owns the breakpoint engine, provides the
/// `WORKSPACE` context (regions read the shared id base + breakpoint), and
/// is the grid container the author styles.
#[derive(Serialize, Deserialize)]
#[component(template = "PineWorkspace.poco", role = "scope")]
pub struct PineWorkspace {
    /// Current viewport tier — engine-driven; emitted on `pp:update:breakpoint`.
    #[model]
    pub breakpoint: String,
    /// Tier assumed before `matchMedia` resolves (and on non-wasm). Default `base`.
    #[prop]
    pub initial: String,
    /// Stable id base; regions derive `{id_base}-{region}` so a Trigger's
    /// `aria-controls` can target them.
    pub id_base: String,
}

impl Default for PineWorkspace {
    fn default() -> Self {
        Self {
            breakpoint: String::new(),
            initial: "base".into(),
            id_base: String::new(),
        }
    }
}

#[handlers]
impl PineWorkspace {
    fn on_setup(&mut self) {
        if let Some(scope) = pocopine::current_scope_id() {
            self.id_base = format!("pine-workspace-{}", scope.0);
        }
        if self.breakpoint.is_empty() {
            self.breakpoint = self.initial.clone();
        }
        WORKSPACE.provide(this::<Self>());
    }

    fn on_ready(&self, handle: Handle<Self>) {
        let sink = handle;
        crate::breakpoint::install(std::rc::Rc::new(move |bp| {
            sink.update(|s| s.breakpoint = bp.as_str().to_string());
        }));
    }
}

/// Read the workspace's id base from context, suffixing `region`.
fn region_id(region: &str) -> String {
    WORKSPACE
        .inject()
        .map(|w| w.with(|s| format!("{}-{region}", s.id_base)))
        .unwrap_or_default()
}

// ── Main ──────────────────────────────────────────────────────────

/// The primary content region — the flex-fill remainder. Renders a
/// `role="main"` landmark.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineWorkspaceMain.poco", role = "panel")]
pub struct PineWorkspaceMain {}

#[handlers]
impl PineWorkspaceMain {}

// ── Sidebar — resizable primary nav with rail-collapse + flyout ────

/// Primary navigation column. Resizable by a grab handle, collapses to a
/// thin **rail** (below the `collapse_at` drag threshold or via toggle / a
/// narrow viewport), and shows a hover **flyout** while collapsed. Publishes
/// its width as `--pine-sidebar-size`.
#[derive(Serialize, Deserialize)]
#[component(template = "PineWorkspaceSidebar.poco", role = "panel")]
pub struct PineWorkspaceSidebar {
    /// Accessible name for the navigation landmark.
    #[prop]
    pub label: String,
    /// Expanded width (px). Two-way via `pp-model:size` for persistence.
    #[model]
    pub size: f64,
    /// Whether collapsed to the rail. Two-way via `pp-model:collapsed`.
    #[model]
    pub collapsed: bool,
    /// Rail width when collapsed (px). Default 56.
    #[prop]
    pub rail_size: f64,
    /// Max expanded width (px). Default 480.
    #[prop]
    pub max: f64,
    /// Release width below which the drag snaps to the rail (px). Default 160.
    #[prop]
    pub collapse_at: f64,
    /// Reset-to (double-click) / initial width (px). Default 260.
    #[prop]
    pub default: f64,
    /// Tier below which the sidebar auto-collapses to the rail. Default `lg`.
    #[prop]
    pub collapse_below: String,
    /// Responsive override — set true while the viewport is below
    /// `collapse_below`. Kept separate from the user/`pp-model` `collapsed`
    /// so the two never race; the rendered state is the OR of both.
    pub responsive_rail: bool,
    /// Rendered collapsed state — `collapsed || responsive_rail`.
    pub display_collapsed: bool,
    /// Rendered width — `rail_size` when collapsed, else `size`.
    pub display_size: f64,
    /// Hover-flyout open (only meaningful while collapsed).
    pub flyout: bool,
    /// Mid-drag flag (drives `data-dragging` so author CSS can suppress
    /// transitions during the drag).
    pub dragging: bool,
    /// Landmark id (Trigger `aria-controls` target).
    pub id: String,
    /// Own scope id.
    pub scope: u64,
}

impl Default for PineWorkspaceSidebar {
    fn default() -> Self {
        let mut this = Self {
            label: String::new(),
            size: 260.0,
            collapsed: false,
            rail_size: 56.0,
            max: 480.0,
            collapse_at: 160.0,
            default: 260.0,
            collapse_below: "lg".into(),
            responsive_rail: false,
            display_collapsed: false,
            display_size: 260.0,
            flyout: false,
            dragging: false,
            id: String::new(),
            scope: 0,
        };
        this.sync_display();
        this
    }
}

impl PineWorkspaceSidebar {
    fn sync_display(&mut self) {
        self.display_collapsed = self.collapsed || self.responsive_rail;
        self.display_size = if self.display_collapsed {
            self.rail_size
        } else {
            self.size
        };
    }

    /// Apply a released drag width: snap to the rail below `collapse_at`,
    /// else keep the (clamped) expanded width.
    fn commit_resize(&mut self, raw: f64) {
        self.dragging = false;
        if raw < self.collapse_at {
            self.collapsed = true;
        } else {
            self.collapsed = false;
            self.size = raw.clamp(self.collapse_at, self.max);
        }
        self.sync_display();
    }

    fn set_size(&mut self, px: f64) {
        self.collapsed = false;
        self.size = px.clamp(self.collapse_at, self.max);
        self.sync_display();
    }
}

#[handlers]
impl PineWorkspaceSidebar {
    fn on_setup(&mut self) {
        if let Some(scope) = pocopine::current_scope_id() {
            self.scope = scope.0;
        }
        self.id = region_id("sidebar");
        self.sync_display();
    }

    fn on_ready(&self, handle: Handle<Self>) {
        // Auto-collapse to the rail below `collapse_below`.
        let Some(ws) = WORKSPACE.inject() else {
            return;
        };
        let ws_scope = ws.scope_id();
        let sink = handle;
        pocopine::watch_scope_field_scoped::<String, _>(ws_scope, "breakpoint", move |bp, _| {
            let below = sink.with(|s| {
                let cur = crate::breakpoint::Breakpoint::from_token(bp)
                    .unwrap_or(crate::breakpoint::Breakpoint::Base);
                let thresh = crate::breakpoint::Breakpoint::from_token(&s.collapse_below)
                    .unwrap_or(crate::breakpoint::Breakpoint::Lg);
                cur.rank() < thresh.rank()
            });
            sink.update(|s| {
                s.responsive_rail = below;
                s.sync_display();
            });
        });
    }

    /// Keep the rendered state in sync when the user/`pp-model` toggles
    /// `collapsed`.
    #[watch(collapsed)]
    fn on_collapsed(&mut self, _new: bool, _old: Option<bool>) {
        self.sync_display();
    }

    /// Begin a pointer-drag resize from the grab handle.
    pub fn on_resize_start(&mut self, ev: wasm_bindgen::JsValue) {
        if self.display_collapsed {
            return;
        }
        use wasm_bindgen::JsCast;
        let Ok(ev) = ev.dyn_into::<web_sys::PointerEvent>() else {
            return;
        };
        ev.prevent_default();
        let start = ev.client_x() as f64;
        let (size, rail, max) = (self.size, self.rail_size, self.max);
        let h_size = this::<Self>();
        let h_commit = h_size.clone();
        resize::start_drag(
            Axis::Horizontal,
            false,
            start,
            size,
            rail,
            max,
            None,
            std::rc::Rc::new(move |px| {
                h_size.update(|s| {
                    s.dragging = true;
                    s.size = px;
                    s.sync_display();
                });
            }),
            std::rc::Rc::new(move |px| h_commit.update(|s| s.commit_resize(px))),
        );
    }

    pub fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
        self.sync_display();
    }
    pub fn nudge_grow(&mut self) {
        self.set_size(self.size + 16.0);
    }
    pub fn nudge_shrink(&mut self) {
        self.set_size(self.size - 16.0);
    }
    pub fn to_min(&mut self) {
        self.set_size(self.collapse_at);
    }
    pub fn to_max(&mut self) {
        self.set_size(self.max);
    }
    pub fn reset_size(&mut self) {
        self.set_size(self.default);
    }
    pub fn on_hover(&mut self) {
        if self.display_collapsed {
            self.flyout = true;
        }
    }
    pub fn on_unhover(&mut self) {
        self.flyout = false;
    }
}

// ── Aside — trailing detail panel: resizable docked column ────────

/// Trailing contextual/detail panel (task detail, AI, comments) — a
/// resizable, collapsible in-grid column. App-controlled via
/// `pp-model:open`; publishes its width as `--pine-aside-size`.
#[derive(Serialize, Deserialize)]
#[component(template = "PineWorkspaceAside.poco", role = "panel")]
pub struct PineWorkspaceAside {
    /// Accessible name for the complementary landmark.
    #[prop]
    pub label: String,
    /// Anchor edge — `end` (right, default) or `start` (left).
    #[prop]
    pub side: String,
    /// Min / max / reset-to width (px).
    #[prop]
    pub min: f64,
    #[prop]
    pub max: f64,
    #[prop]
    pub default: f64,
    /// Whether the panel is shown. Two-way via `pp-model:open`.
    #[model]
    pub open: bool,
    /// Width (px). Two-way via `pp-model:size`.
    #[model]
    pub size: f64,
    /// Rendered width (mirrors `size`).
    pub display_size: f64,
    /// Mid-drag flag.
    pub dragging: bool,
    /// Landmark id.
    pub id: String,
}

impl Default for PineWorkspaceAside {
    fn default() -> Self {
        Self {
            label: String::new(),
            side: "end".into(),
            min: 240.0,
            max: 560.0,
            default: 320.0,
            open: false,
            size: 320.0,
            display_size: 320.0,
            dragging: false,
            id: String::new(),
        }
    }
}

impl PineWorkspaceAside {
    /// End-anchored (right) grows as the pointer moves left.
    fn invert(&self) -> bool {
        self.side != "start"
    }

    fn set_size(&mut self, px: f64) {
        self.size = px.clamp(self.min, self.max);
        self.display_size = self.size;
    }
}

#[handlers]
impl PineWorkspaceAside {
    fn on_setup(&mut self) {
        self.id = region_id("aside");
        self.display_size = self.size;
    }

    pub fn on_resize_start(&mut self, ev: wasm_bindgen::JsValue) {
        use wasm_bindgen::JsCast;
        let Ok(ev) = ev.dyn_into::<web_sys::PointerEvent>() else {
            return;
        };
        ev.prevent_default();
        let start = ev.client_x() as f64;
        let (size, min, max, invert) = (self.size, self.min, self.max, self.invert());
        let h_size = this::<Self>();
        let h_commit = h_size.clone();
        resize::start_drag(
            Axis::Horizontal,
            invert,
            start,
            size,
            min,
            max,
            None,
            std::rc::Rc::new(move |px| {
                h_size.update(|s| {
                    s.dragging = true;
                    s.set_size(px);
                });
            }),
            std::rc::Rc::new(move |px| {
                h_commit.update(|s| {
                    s.dragging = false;
                    s.set_size(px);
                });
            }),
        );
    }

    pub fn nudge_grow(&mut self) {
        self.set_size(self.size + 16.0);
    }
    pub fn nudge_shrink(&mut self) {
        self.set_size(self.size - 16.0);
    }
    pub fn to_min(&mut self) {
        self.set_size(self.min);
    }
    pub fn to_max(&mut self) {
        self.set_size(self.max);
    }
    pub fn reset_size(&mut self) {
        self.set_size(self.default);
    }
}

// ── Header / Footer — landmark bars ────────────────────────────────

/// Top app-bar / banner. Renders a `role="banner"` landmark.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineWorkspaceHeader.poco", role = "panel")]
pub struct PineWorkspaceHeader {}

#[handlers]
impl PineWorkspaceHeader {}

/// Status bar / footer. Renders a `role="contentinfo"` landmark.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineWorkspaceFooter.poco", role = "panel")]
pub struct PineWorkspaceFooter {}

#[handlers]
impl PineWorkspaceFooter {}

// ── Outer Panel — collapsible secondary nav (start / end) ──────────

/// Outer secondary-nav column (Atlassian LeftPanel/RightPanel, VS Code
/// activity-bar region). Collapsible via `pp-model:open`; `side` picks the
/// edge. Fixed-width by author CSS (resize is the Sidebar/Aside's job).
#[derive(Serialize, Deserialize)]
#[component(template = "PineWorkspacePanel.poco", role = "panel")]
pub struct PineWorkspacePanel {
    /// `start` (left, default) or `end` (right).
    #[prop]
    pub side: String,
    /// Accessible name for the navigation landmark.
    #[prop]
    pub label: String,
    /// Shown / hidden. Two-way via `pp-model:open`.
    #[model]
    pub open: bool,
    /// Landmark id.
    pub id: String,
}

impl Default for PineWorkspacePanel {
    fn default() -> Self {
        Self {
            side: "start".into(),
            label: String::new(),
            open: true,
            id: String::new(),
        }
    }
}

#[handlers]
impl PineWorkspacePanel {
    fn on_setup(&mut self) {
        let suffix = if self.side == "end" {
            "panel-end"
        } else {
            "panel-start"
        };
        self.id = region_id(suffix);
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

// ── Bottom Panel — terminal/console: vertical resize + collapse ────

/// Bottom (or top) panel — terminal/console/output. Resizes vertically and
/// collapses. `edge` relocates it (author CSS keys on `data-edge`).
/// Publishes its height as `--pine-bottom-size`.
#[derive(Serialize, Deserialize)]
#[component(template = "PineWorkspaceBottom.poco", role = "panel")]
pub struct PineWorkspaceBottom {
    /// Accessible name for the complementary landmark.
    #[prop]
    pub label: String,
    /// `bottom` (default) or `top`.
    #[prop]
    pub edge: String,
    /// Min / max / reset-to height (px).
    #[prop]
    pub min: f64,
    #[prop]
    pub max: f64,
    #[prop]
    pub default: f64,
    /// Shown / hidden. Two-way via `pp-model:open`.
    #[model]
    pub open: bool,
    /// Height (px). Two-way via `pp-model:size`.
    #[model]
    pub size: f64,
    /// Rendered height (mirrors `size`).
    pub display_size: f64,
    /// Mid-drag flag.
    pub dragging: bool,
    /// Landmark id.
    pub id: String,
}

impl Default for PineWorkspaceBottom {
    fn default() -> Self {
        Self {
            label: String::new(),
            edge: "bottom".into(),
            min: 120.0,
            max: 600.0,
            default: 240.0,
            open: false,
            size: 240.0,
            display_size: 240.0,
            dragging: false,
            id: String::new(),
        }
    }
}

impl PineWorkspaceBottom {
    fn set_size(&mut self, px: f64) {
        self.size = px.clamp(self.min, self.max);
        self.display_size = self.size;
    }
}

#[handlers]
impl PineWorkspaceBottom {
    fn on_setup(&mut self) {
        self.id = region_id("bottom");
        self.display_size = self.size;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn on_resize_start(&mut self, ev: wasm_bindgen::JsValue) {
        use wasm_bindgen::JsCast;
        let Ok(ev) = ev.dyn_into::<web_sys::PointerEvent>() else {
            return;
        };
        ev.prevent_default();
        let start = ev.client_y() as f64;
        // Bottom-docked: handle on top edge, dragging up (negative dy) grows.
        let invert = self.edge != "top";
        let (size, min, max) = (self.size, self.min, self.max);
        let h_size = this::<Self>();
        let h_commit = h_size.clone();
        resize::start_drag(
            Axis::Vertical,
            invert,
            start,
            size,
            min,
            max,
            None,
            std::rc::Rc::new(move |px| {
                h_size.update(|s| {
                    s.dragging = true;
                    s.set_size(px);
                });
            }),
            std::rc::Rc::new(move |px| {
                h_commit.update(|s| {
                    s.dragging = false;
                    s.set_size(px);
                });
            }),
        );
    }

    pub fn nudge_grow(&mut self) {
        self.set_size(self.size + 16.0);
    }
    pub fn nudge_shrink(&mut self) {
        self.set_size(self.size - 16.0);
    }
    pub fn to_min(&mut self) {
        self.set_size(self.min);
    }
    pub fn to_max(&mut self) {
        self.set_size(self.max);
    }
    pub fn reset_size(&mut self) {
        self.set_size(self.default);
    }
}
