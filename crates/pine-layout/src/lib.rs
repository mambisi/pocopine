//! Pine Layout — headless, responsive layout primitives for pocopine.
//!
//! Pine Layout is to *responsiveness* what core Pine is to behavior:
//! it ships **zero CSS**. Every component exposes a semantic class
//! name (`pine-grid`, `pine-app-shell`) plus `data-*` attributes
//! (`data-breakpoint="md"`, `data-nav-mode="rail"`, `data-cols="12"`)
//! and reactive state — authors do all the actual layout in their own
//! CSS, or with Pine Stylekit utility classes (`grid grid-cols-12
//! md:grid-cols-6`). The library's value is one canonical breakpoint
//! vocabulary and the adaptive-navigation state machine, not opinions
//! about how things look.
//!
//! # Breakpoints
//!
//! The reactive breakpoint resolves to the **same** thresholds Pine
//! Stylekit uses for its `sm:`/`md:`/`lg:`/`xl:`/`2xl:` utility
//! prefixes, so `data-breakpoint == "md"` and a `md:` class never
//! disagree:
//!
//! | tier  | min-width |
//! |-------|-----------|
//! | base  | 0         |
//! | sm    | 640px     |
//! | md    | 768px     |
//! | lg    | 1024px    |
//! | xl    | 1280px    |
//! | 2xl   | 1536px    |
//!
//! # Using Pine Layout
//!
//! ```ignore
//! use pocopine::prelude::*;
//!
//! fn main() {
//!     pine_layout::register_all();
//!     App::new().register::<MyApp>().run();
//! }
//! ```
//!
//! `register_all` registers every Pine Layout component's custom-
//! element tag so authors can drop `<pine-app-shell>`, `<pine-grid>`
//! etc. into their templates.

pub mod app_shell;
pub mod breakpoint;
pub mod container;
pub mod grid;
pub mod inline;
pub mod stack;

pub use app_shell::{
    PineAppShell, PineAppShellContent, PineAppShellFooter, PineAppShellHeader, PineAppShellSidebar,
    PineAppShellTrigger,
};
pub use breakpoint::{Breakpoint, PineBreakpoint};
pub use container::PineContainer;
pub use grid::{PineGrid, PineGridItem};
pub use inline::PineInline;
pub use stack::PineStack;

/// Register every Pine Layout custom-element tag. Call once at
/// startup, before mounting any template that uses the tags.
pub fn register_all() {
    PineBreakpoint::register();
    PineContainer::register();
    PineStack::register();
    PineInline::register();
    PineGrid::register();
    PineGridItem::register();
    PineAppShell::register();
    PineAppShellHeader::register();
    PineAppShellSidebar::register();
    PineAppShellContent::register();
    PineAppShellFooter::register();
    PineAppShellTrigger::register();
}
