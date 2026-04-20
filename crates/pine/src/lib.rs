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
pub mod alert_dialog;
pub mod aspect_ratio;
pub mod avatar;
pub mod button;
pub mod checkbox;
pub mod collapsible;
pub mod context_menu;
pub mod dialog;
pub mod dropdown_menu;
pub mod hover_card;
pub mod label;
pub mod overlay;
pub mod password_toggle_field;
pub mod popover;
pub mod progress;
pub mod radio_group;
pub mod separator;
pub mod switch;
pub mod tabs;
pub mod toggle;
pub mod toggle_group;
pub mod toolbar;
pub mod tooltip;

pub use accordion::{
    PineAccordionContent, PineAccordionItem, PineAccordionRoot, PineAccordionTrigger,
};
pub use alert_dialog::{
    PineAlertDialogAction, PineAlertDialogCancel, PineAlertDialogContent,
    PineAlertDialogDescription, PineAlertDialogOverlay, PineAlertDialogPortal,
    PineAlertDialogRoot, PineAlertDialogTitle, PineAlertDialogTrigger,
};
pub use aspect_ratio::PineAspectRatio;
pub use label::PineLabel;
pub use progress::{PineProgressIndicator, PineProgressRoot};
pub use separator::PineSeparator;
pub use toolbar::{
    PineToolbarButton, PineToolbarLink, PineToolbarRoot, PineToolbarSeparator,
};
pub use avatar::{PineAvatarFallback, PineAvatarImage, PineAvatarRoot};
pub use button::PineButton;
pub use checkbox::PineCheckbox;
pub use collapsible::{PineCollapsibleContent, PineCollapsibleRoot, PineCollapsibleTrigger};
pub use context_menu::{
    PineContextMenuContent, PineContextMenuItem, PineContextMenuPortal, PineContextMenuRoot,
    PineContextMenuSeparator, PineContextMenuTrigger,
};
pub use dialog::{
    PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay,
    PineDialogPortal, PineDialogRoot, PineDialogTitle, PineDialogTrigger,
};
pub use dropdown_menu::{
    PineDropdownMenuArrow, PineDropdownMenuCheckboxItem, PineDropdownMenuContent,
    PineDropdownMenuGroup, PineDropdownMenuItem, PineDropdownMenuItemIndicator,
    PineDropdownMenuLabel, PineDropdownMenuPortal, PineDropdownMenuRadioGroup,
    PineDropdownMenuRadioItem, PineDropdownMenuRoot, PineDropdownMenuSeparator,
    PineDropdownMenuSub, PineDropdownMenuSubContent, PineDropdownMenuSubTrigger,
    PineDropdownMenuTrigger,
};
pub use hover_card::{
    PineHoverCardContent, PineHoverCardPortal, PineHoverCardRoot, PineHoverCardTrigger,
};
pub use password_toggle_field::{
    PinePasswordToggleFieldInput, PinePasswordToggleFieldRoot, PinePasswordToggleFieldToggle,
};
pub use popover::{
    PinePopoverClose, PinePopoverContent, PinePopoverPortal, PinePopoverRoot, PinePopoverTrigger,
};
pub use radio_group::{PineRadioGroupIndicator, PineRadioGroupItem, PineRadioGroupRoot};
pub use switch::PineSwitch;
pub use tabs::{PineTabsContent, PineTabsList, PineTabsRoot, PineTabsTrigger};
pub use toggle::PineToggle;
pub use toggle_group::{PineToggleGroupItem, PineToggleGroupRoot};
pub use tooltip::{
    PineTooltipContent, PineTooltipPortal, PineTooltipProvider, PineTooltipRoot,
    PineTooltipTrigger,
};

/// Register every Pine custom-element tag. Call once at app startup
/// before mounting.
pub fn register_all() {
    PineAccordionRoot::register();
    PineAccordionItem::register();
    PineAccordionTrigger::register();
    PineAccordionContent::register();
    PineAlertDialogRoot::register();
    PineAlertDialogTrigger::register();
    PineAlertDialogPortal::register();
    PineAlertDialogOverlay::register();
    PineAlertDialogContent::register();
    PineAlertDialogTitle::register();
    PineAlertDialogDescription::register();
    PineAlertDialogAction::register();
    PineAlertDialogCancel::register();
    PineAvatarRoot::register();
    PineAvatarImage::register();
    PineAvatarFallback::register();
    PineButton::register();
    PineCollapsibleRoot::register();
    PineCollapsibleTrigger::register();
    PineCollapsibleContent::register();
    PineContextMenuRoot::register();
    PineContextMenuTrigger::register();
    PineContextMenuPortal::register();
    PineContextMenuContent::register();
    PineContextMenuItem::register();
    PineContextMenuSeparator::register();
    PineDialogRoot::register();
    PineDialogTrigger::register();
    PineDialogPortal::register();
    PineDialogOverlay::register();
    PineDialogContent::register();
    PineDialogTitle::register();
    PineDialogDescription::register();
    PineDialogClose::register();
    PinePopoverRoot::register();
    PinePopoverTrigger::register();
    PinePopoverPortal::register();
    PinePopoverContent::register();
    PinePopoverClose::register();
    PineRadioGroupRoot::register();
    PineRadioGroupItem::register();
    PineRadioGroupIndicator::register();
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
    PineDropdownMenuArrow::register();
    PineDropdownMenuSub::register();
    PineDropdownMenuSubTrigger::register();
    PineDropdownMenuSubContent::register();
    PineHoverCardRoot::register();
    PineHoverCardTrigger::register();
    PineHoverCardPortal::register();
    PineHoverCardContent::register();
    PineTabsRoot::register();
    PineTabsList::register();
    PineTabsTrigger::register();
    PineTabsContent::register();
    PineToggle::register();
    PineToggleGroupRoot::register();
    PineToggleGroupItem::register();
    PineTooltipProvider::register();
    PineTooltipRoot::register();
    PineTooltipTrigger::register();
    PineTooltipPortal::register();
    PineTooltipContent::register();
    PineSwitch::register();
    PineCheckbox::register();
    PineLabel::register();
    PineSeparator::register();
    PineProgressRoot::register();
    PineProgressIndicator::register();
    PineAspectRatio::register();
    PineToolbarRoot::register();
    PineToolbarButton::register();
    PineToolbarLink::register();
    PineToolbarSeparator::register();
    PinePasswordToggleFieldRoot::register();
    PinePasswordToggleFieldInput::register();
    PinePasswordToggleFieldToggle::register();
}
