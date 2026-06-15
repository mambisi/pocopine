//! `<pine-app-shell-*>` — adaptive application scaffold (compound).
//!
//! Six parts: a **Root** that owns the breakpoint engine + the
//! adaptive-navigation state machine, and five regions
//! (**Header**, **Sidebar**, **Content**, **Footer**, **Trigger**).
//!
//! The Root resolves the viewport tier and derives a `nav_mode` from
//! it — `drawer` (narrow), `rail` (medium), or `sidebar` (wide),
//! mirroring Material's bottom-bar → rail → drawer progression and
//! Apple's collapse-to-stack-in-compact behavior. It surfaces
//! `data-breakpoint` + `data-nav-mode` on its element and provides the
//! `SHELL` context; the regions read it. **Headless** — the author's
//! CSS decides what each `nav_mode` looks like:
//!
//! ```css
//! .pine-app-shell { display: grid; }
//! .pine-app-shell[data-nav-mode="drawer"]  { grid-template-columns: 1fr; }
//! .pine-app-shell[data-nav-mode="rail"]    { grid-template-columns: 4rem 1fr; }
//! .pine-app-shell[data-nav-mode="sidebar"] { grid-template-columns: 16rem 1fr; }
//! /* the drawer slides in only in drawer mode: */
//! .pine-app-shell-sidebar[data-nav-mode="drawer"]               { position: fixed; transform: translateX(-100%); }
//! .pine-app-shell-sidebar[data-nav-mode="drawer"][data-state="open"] { transform: none; }
//! ```
//!
//! ```html
//! <pine-app-shell pp-model:breakpoint="bp">
//!   <pine-app-shell-header>
//!     <pine-app-shell-trigger>☰</pine-app-shell-trigger>
//!   </pine-app-shell-header>
//!   <pine-app-shell-sidebar label="Main">…nav…</pine-app-shell-sidebar>
//!   <pine-app-shell-content>…page…</pine-app-shell-content>
//!   <pine-app-shell-footer>…</pine-app-shell-footer>
//! </pine-app-shell>
//! ```

use crate::breakpoint::{Breakpoint, install};
use crate::floating as drawer;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

create_context!(SHELL: Handle<PineAppShell>);

// ── Root ──────────────────────────────────────────────────────────

/// The application scaffold root. Owns the breakpoint engine and the
/// adaptive-navigation state machine. See module docs.
#[derive(Serialize, Deserialize)]
#[component(template = "PineAppShell.poco", role = "scope")]
pub struct PineAppShell {
    /// Current viewport tier — `base`/`sm`/`md`/`lg`/`xl`/`2xl`.
    /// Engine-driven; emitted on `pp:update:breakpoint`.
    #[model]
    pub breakpoint: String,
    /// Whether the drawer is open. Only meaningful while
    /// `nav_mode == "drawer"`. Emitted on `pp:update:nav_open`.
    #[model]
    pub nav_open: bool,
    /// Derived navigation mode — `drawer`/`rail`/`sidebar`.
    pub nav_mode: String,
    /// Smallest tier at which navigation becomes a persistent rail.
    /// Defaults to `md`.
    #[prop]
    pub rail_at: String,
    /// Smallest tier at which navigation becomes a full sidebar.
    /// Defaults to `xl`.
    #[prop]
    pub sidebar_at: String,
    /// Tier assumed before `matchMedia` resolves (and the permanent
    /// value on non-wasm / SSR). Defaults to `base`.
    #[prop]
    pub initial: String,
    /// Stable id for the navigation landmark, used to wire the
    /// Trigger's `aria-controls` to the Sidebar.
    pub nav_id: String,
}

impl Default for PineAppShell {
    fn default() -> Self {
        Self {
            breakpoint: String::new(),
            nav_open: false,
            nav_mode: "drawer".into(),
            rail_at: "md".into(),
            sidebar_at: "xl".into(),
            initial: "base".into(),
            nav_id: String::new(),
        }
    }
}

impl PineAppShell {
    /// Recompute `nav_mode` from the current breakpoint and the
    /// `rail_at` / `sidebar_at` thresholds. Closes the drawer when the
    /// viewport grows out of drawer mode so focus/scroll are released.
    fn recompute_nav_mode(&mut self) {
        self.nav_mode =
            crate::breakpoint::nav_mode(&self.breakpoint, &self.rail_at, &self.sidebar_at)
                .to_string();
        if self.nav_mode != "drawer" && self.nav_open {
            // Leaving drawer mode — the Sidebar's watcher releases the
            // focus trap + scroll lock when it sees this flip to false.
            self.nav_open = false;
        }
    }

    /// Toggle the drawer (no-op outside drawer mode).
    pub fn toggle_nav(&mut self) {
        if self.nav_mode == "drawer" {
            self.nav_open = !self.nav_open;
        }
    }

    /// Open the drawer (no-op outside drawer mode).
    pub fn open_nav(&mut self) {
        if self.nav_mode == "drawer" {
            self.nav_open = true;
        }
    }

    /// Close the drawer.
    pub fn close_nav(&mut self) {
        self.nav_open = false;
    }
}

#[handlers]
impl PineAppShell {
    fn on_setup(&mut self) {
        if let Some(scope) = pocopine::current_scope_id() {
            self.nav_id = format!("pine-app-shell-nav-{}", scope.0);
        }
        if self.breakpoint.is_empty() {
            self.breakpoint = self.initial.clone();
        }
        self.recompute_nav_mode();
        SHELL.provide(this::<Self>());
    }

    fn on_ready(&self, handle: Handle<Self>) {
        let sink = handle;
        install(std::rc::Rc::new(move |bp: Breakpoint| {
            sink.update(|s| {
                s.breakpoint = bp.as_str().to_string();
                s.recompute_nav_mode();
            });
        }));
    }
}

// ── Header / Content / Footer — landmark regions ──────────────────

/// Top app-bar region. Renders a `role="banner"` landmark.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAppShellHeader.poco", role = "panel")]
pub struct PineAppShellHeader {}

#[handlers]
impl PineAppShellHeader {}

/// Main content region. Renders a `role="main"` landmark.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAppShellContent.poco", role = "panel")]
pub struct PineAppShellContent {}

#[handlers]
impl PineAppShellContent {}

/// Footer region. Renders a `role="contentinfo"` landmark.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAppShellFooter.poco", role = "panel")]
pub struct PineAppShellFooter {}

#[handlers]
impl PineAppShellFooter {}

// ── Sidebar — the adaptive navigation region ──────────────────────

/// Primary navigation region. Renders a `role="navigation"` landmark
/// carrying `data-nav-mode` + `data-state`. In `drawer` mode it
/// behaves like a modal: Escape closes it, focus is trapped while
/// open, and the page scroll is locked. (Scrim / outside-click
/// dismissal is deferred to v2 — a capture-phase `@click.outside` on
/// the sidebar would treat the toggle as "outside" and fight it.)
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAppShellSidebar.poco", role = "panel")]
pub struct PineAppShellSidebar {
    /// Accessible name for the navigation landmark.
    #[prop]
    pub label: String,
    /// Mirrored from the shell — drives `data-nav-mode`.
    #[observe(SHELL)]
    pub nav_mode: String,
    /// Mirrored from the shell — drives `data-state` (open/closed).
    #[observe(SHELL)]
    pub nav_open: bool,
    /// Mirrored from the shell — the navigation landmark's `id`
    /// (the Trigger's `aria-controls` target).
    #[observe(SHELL)]
    pub nav_id: String,
    /// This sidebar's own scope id, captured at setup as the drawer
    /// side-table key.
    pub scope: u64,
}

#[handlers]
impl PineAppShellSidebar {
    fn on_setup(&mut self) {
        if let Some(scope) = pocopine::current_scope_id() {
            self.scope = scope.0;
        }
    }

    fn on_ready(&self, handle: Handle<Self>, refs: pocopine::Refs) {
        let Some(el) = refs.get("sidebar") else {
            return;
        };
        let Some(shell) = SHELL.inject() else {
            return;
        };
        let shell_scope = shell.scope_id();
        let scope = handle.with(|s| s.scope);

        // Drive the focus trap + scroll lock off the shell's `nav_open`
        // (proven cross-scope watch, like Splitter watches `sizes`).
        let el_for_watch = el.clone();
        let shell_for_watch = shell.clone();
        pocopine::watch_scope_field_scoped::<bool, _>(shell_scope, "nav_open", move |open, _| {
            let mode = shell_for_watch.with(|s| s.nav_mode.clone());
            if *open && mode == "drawer" {
                drawer::open(scope, &el_for_watch);
            } else {
                drawer::close(scope);
            }
        });

        // Release the drawer if the sidebar unmounts while open.
        pocopine::on_scope_unmount(move || drawer::close(scope));
    }

    /// Close the drawer (Escape). No-op outside drawer mode so a
    /// visible rail/sidebar isn't affected by stray key events.
    pub fn close_nav(&mut self) {
        if self.nav_mode != "drawer" {
            return;
        }
        if let Some(shell) = SHELL.inject() {
            shell.update(|s| s.close_nav());
        }
    }
}

// ── Trigger — the hamburger toggle ────────────────────────────────

/// A `<button>` that toggles the drawer. Mirrors the drawer state onto
/// `aria-expanded` and points `aria-controls` at the Sidebar.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAppShellTrigger.poco", role = "interactive")]
pub struct PineAppShellTrigger {
    /// Mirrored from the shell — drives `aria-expanded`.
    #[observe(SHELL)]
    pub nav_open: bool,
    /// Mirrored from the shell — drives `data-nav-mode` (so authors can
    /// hide the trigger outside drawer mode).
    #[observe(SHELL)]
    pub nav_mode: String,
    /// Mirrored from the shell — the `aria-controls` target id.
    #[observe(SHELL)]
    pub nav_id: String,
}

#[handlers]
impl PineAppShellTrigger {
    pub fn toggle(&mut self) {
        if let Some(shell) = SHELL.inject() {
            shell.update(|s| s.toggle_nav());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell at tier `bp` with the given `rail_at`/`sidebar_at`
    /// thresholds, nav_mode recomputed.
    fn shell(bp: &str, rail_at: &str, sidebar_at: &str) -> PineAppShell {
        let mut s = PineAppShell {
            breakpoint: bp.into(),
            rail_at: rail_at.into(),
            sidebar_at: sidebar_at.into(),
            ..PineAppShell::default()
        };
        s.recompute_nav_mode();
        s
    }

    #[test]
    fn nav_mode_default_thresholds() {
        // rail_at = md, sidebar_at = xl (the defaults).
        let expect = [
            ("base", "drawer"),
            ("sm", "drawer"),
            ("md", "rail"),
            ("lg", "rail"),
            ("xl", "sidebar"),
            ("2xl", "sidebar"),
        ];
        for (bp, mode) in expect {
            assert_eq!(shell(bp, "md", "xl").nav_mode, mode, "at {bp}");
        }
    }

    #[test]
    fn nav_mode_custom_thresholds() {
        // Rail only from lg, full sidebar only at 2xl.
        let expect = [
            ("md", "drawer"),
            ("lg", "rail"),
            ("xl", "rail"),
            ("2xl", "sidebar"),
        ];
        for (bp, mode) in expect {
            assert_eq!(shell(bp, "lg", "2xl").nav_mode, mode, "at {bp}");
        }
    }

    #[test]
    fn toggle_only_acts_in_drawer_mode() {
        let mut drawer = shell("base", "md", "xl");
        assert_eq!(drawer.nav_mode, "drawer");
        assert!(!drawer.nav_open);
        drawer.toggle_nav();
        assert!(drawer.nav_open, "drawer toggles open");
        drawer.toggle_nav();
        assert!(!drawer.nav_open, "drawer toggles closed");

        let mut rail = shell("md", "md", "xl");
        assert_eq!(rail.nav_mode, "rail");
        rail.toggle_nav();
        assert!(!rail.nav_open, "rail mode ignores toggle");
    }

    #[test]
    fn open_close_semantics() {
        let mut s = shell("base", "md", "xl");
        s.open_nav();
        assert!(s.nav_open);
        s.close_nav();
        assert!(!s.nav_open);

        let mut rail = shell("xl", "md", "xl");
        rail.open_nav();
        assert!(!rail.nav_open, "open_nav is a no-op outside drawer mode");
    }

    #[test]
    fn growing_out_of_drawer_closes_the_drawer() {
        let mut s = shell("base", "md", "xl");
        s.open_nav();
        assert!(s.nav_open);
        // Viewport grows to lg → rail mode; the open drawer must close
        // so focus/scroll get released.
        s.breakpoint = "lg".into();
        s.recompute_nav_mode();
        assert_eq!(s.nav_mode, "rail");
        assert!(!s.nav_open, "drawer auto-closes when it leaves drawer mode");
    }
}
