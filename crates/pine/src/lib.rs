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

pub mod accordion;
pub mod avatar;
pub mod button;
pub mod checkbox;
pub mod collapsible;
pub mod dialog;
pub mod dropdown_menu;
pub mod popover;
pub mod switch;
pub mod tabs;
pub mod tooltip;

pub use accordion::{
    PineAccordionContent, PineAccordionItem, PineAccordionRoot, PineAccordionTrigger,
};
pub use avatar::{PineAvatarFallback, PineAvatarImage, PineAvatarRoot};
pub use button::PineButton;
pub use checkbox::PineCheckbox;
pub use collapsible::{PineCollapsibleContent, PineCollapsibleRoot, PineCollapsibleTrigger};
pub use dialog::PineDialog;
pub use dropdown_menu::{
    PineDropdownMenuCheckboxItem, PineDropdownMenuContent, PineDropdownMenuGroup,
    PineDropdownMenuItem, PineDropdownMenuItemIndicator, PineDropdownMenuLabel,
    PineDropdownMenuPortal, PineDropdownMenuRadioGroup, PineDropdownMenuRadioItem,
    PineDropdownMenuRoot, PineDropdownMenuSeparator, PineDropdownMenuTrigger,
};
pub use popover::PinePopover;
pub use switch::PineSwitch;
pub use tabs::{PineTabs, TabDef};
pub use tooltip::PineTooltip;

/// Register every Pine custom-element tag. Call once at app startup
/// before mounting.
pub fn register_all() {
    PineAccordionRoot::register();
    PineAccordionItem::register();
    PineAccordionTrigger::register();
    PineAccordionContent::register();
    PineAvatarRoot::register();
    PineAvatarImage::register();
    PineAvatarFallback::register();
    PineButton::register();
    PineCollapsibleRoot::register();
    PineCollapsibleTrigger::register();
    PineCollapsibleContent::register();
    PineDialog::register();
    PinePopover::register();
    PineDropdownMenuRoot::register();
    PineDropdownMenuTrigger::register();
    PineDropdownMenuPortal::register();
    PineDropdownMenuContent::register();
    PineDropdownMenuItem::register();
    PineDropdownMenuSeparator::register();
    PineDropdownMenuGroup::register();
    PineDropdownMenuLabel::register();
    PineDropdownMenuCheckboxItem::register();
    PineDropdownMenuItemIndicator::register();
    PineDropdownMenuRadioGroup::register();
    PineDropdownMenuRadioItem::register();
    PineTabs::register();
    PineTooltip::register();
    PineSwitch::register();
    PineCheckbox::register();
}
