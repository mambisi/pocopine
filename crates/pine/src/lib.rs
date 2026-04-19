//! Pine — unstyled, accessible UI primitives for pocopine.
//!
//! Pine ships **behavior, keyboard, focus, and ARIA**. It ships
//! **zero CSS**. Each component exposes semantic class names
//! (`pine-dialog`, `pine-tabs-list`) plus `data-*` state attributes
//! (`data-state="open"`, `data-variant="primary"`); authors style
//! via their own CSS.
//!
//! # Using Pine
//!
//! ```ignore
//! use pocopine::prelude::*;
//!
//! fn main() {
//!     pine::register_all();
//!     App::new()
//!         .register::<MyApp>()
//!         .run();
//! }
//! ```
//!
//! `register_all` registers every Pine component's custom-element
//! tag so authors can drop `<pine-dialog>`, `<pine-button>` etc.
//! into their templates without enumerating the library.

pub mod button;
pub mod checkbox;
pub mod dialog;
pub mod dropdown_menu;
pub mod popover;
pub mod switch;
pub mod tabs;
pub mod tooltip;

pub use button::PineButton;
pub use checkbox::PineCheckbox;
pub use dialog::PineDialog;
pub use dropdown_menu::{
    PineDropdownMenuContent, PineDropdownMenuItem, PineDropdownMenuPortal,
    PineDropdownMenuRoot, PineDropdownMenuTrigger,
};
pub use popover::PinePopover;
pub use switch::PineSwitch;
pub use tabs::{PineTabs, TabDef};
pub use tooltip::PineTooltip;

/// Register every Pine custom-element tag. Call once at app startup
/// before mounting.
pub fn register_all() {
    PineButton::register();
    PineDialog::register();
    PinePopover::register();
    PineDropdownMenuRoot::register();
    PineDropdownMenuTrigger::register();
    PineDropdownMenuPortal::register();
    PineDropdownMenuContent::register();
    PineDropdownMenuItem::register();
    PineTabs::register();
    PineTooltip::register();
    PineSwitch::register();
    PineCheckbox::register();
}
