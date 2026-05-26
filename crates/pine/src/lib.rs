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
pub mod calendar;
pub mod checkbox;
pub mod collapsible;
pub mod combobox;
pub mod command;
pub mod compound;
pub mod context_menu;
pub mod date_field;
pub mod date_picker;
pub mod date_range_field;
pub mod date_range_picker;
pub mod datetime;
pub mod dialog;
pub mod dropdown_menu;
pub mod field;
pub mod fieldset;
pub mod form;
pub mod hover_card;
pub mod input;
pub mod label;
pub mod otp_field;
pub mod overlay;
pub mod password_toggle_field;
pub mod popover;
pub mod progress;
pub mod radio_group;
pub mod range_calendar;
pub mod scroll_area;
pub mod select;
pub mod separator;
pub mod slider;
pub mod splitter;
pub mod switch;
pub mod tabs;
pub mod tags_input;
pub mod text;
pub mod textarea;
pub mod time_field;
pub mod time_range_field;
pub mod toggle;
pub mod toggle_group;
pub mod toolbar;
pub mod tooltip;
pub mod tree;
pub mod upload;

pub use accordion::{
    PineAccordionContent, PineAccordionItem, PineAccordionRoot, PineAccordionTrigger,
};
pub use alert_dialog::{
    PineAlertDialogAction, PineAlertDialogCancel, PineAlertDialogContent,
    PineAlertDialogDescription, PineAlertDialogOverlay, PineAlertDialogPortal, PineAlertDialogRoot,
    PineAlertDialogTitle, PineAlertDialogTrigger,
};
pub use aspect_ratio::PineAspectRatio;
pub use avatar::{PineAvatarFallback, PineAvatarImage, PineAvatarRoot};
pub use button::PineButton;
pub use calendar::{
    PineCalendarCell, PineCalendarCellTrigger, PineCalendarGrid, PineCalendarGridBody,
    PineCalendarGridHead, PineCalendarGridRow, PineCalendarHeadCell, PineCalendarHeader,
    PineCalendarHeading, PineCalendarNext, PineCalendarPrev, PineCalendarRoot,
};
pub use checkbox::PineCheckbox;
pub use collapsible::{PineCollapsibleContent, PineCollapsibleRoot, PineCollapsibleTrigger};
pub use combobox::{
    PineComboboxContent, PineComboboxEmpty, PineComboboxInput, PineComboboxItem,
    PineComboboxPortal, PineComboboxRoot,
};
pub use command::{
    PineCommandContent, PineCommandEmpty, PineCommandInput, PineCommandItem, PineCommandList,
    PineCommandOverlay, PineCommandPortal, PineCommandRoot,
};
pub use context_menu::{
    PineContextMenuContent, PineContextMenuItem, PineContextMenuPortal, PineContextMenuRoot,
    PineContextMenuSeparator, PineContextMenuTrigger,
};
pub use date_field::PineDateField;
pub use date_picker::PineDatePicker;
pub use date_range_field::PineDateRangeField;
pub use date_range_picker::PineDateRangePicker;
pub use dialog::{
    PineDialog, PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay,
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
pub use field::{
    PineFieldControl, PineFieldDescription, PineFieldError, PineFieldLabel, PineFieldRoot,
};
pub use fieldset::{PineFieldsetLegend, PineFieldsetRoot};
pub use form::PineFormRoot;
pub use hover_card::{
    PineHoverCardContent, PineHoverCardPortal, PineHoverCardRoot, PineHoverCardTrigger,
};
pub use input::PineInput;
pub use label::PineLabel;
pub use otp_field::PineOtpField;
pub use password_toggle_field::{
    PinePasswordToggleFieldInput, PinePasswordToggleFieldRoot, PinePasswordToggleFieldToggle,
};
pub use popover::{
    PinePopoverClose, PinePopoverContent, PinePopoverPortal, PinePopoverRoot, PinePopoverTrigger,
};
pub use progress::{PineProgressIndicator, PineProgressRoot};
pub use radio_group::{PineRadioGroupIndicator, PineRadioGroupItem, PineRadioGroupRoot};
pub use range_calendar::{
    PineRangeCalendarCell, PineRangeCalendarCellTrigger, PineRangeCalendarGrid,
    PineRangeCalendarGridBody, PineRangeCalendarGridHead, PineRangeCalendarHeader,
    PineRangeCalendarHeading, PineRangeCalendarNext, PineRangeCalendarPrev, PineRangeCalendarRoot,
};
pub use scroll_area::{
    PineScrollAreaCorner, PineScrollAreaRoot, PineScrollAreaScrollbar, PineScrollAreaThumb,
    PineScrollAreaViewport,
};
pub use select::{
    PineSelectContent, PineSelectItem, PineSelectItemIndicator, PineSelectPortal, PineSelectRoot,
    PineSelectSeparator, PineSelectTrigger, PineSelectValue,
};
pub use separator::PineSeparator;
pub use slider::{PineSliderRange, PineSliderRoot, PineSliderThumb, PineSliderTrack};
pub use splitter::{PineSplitterGroup, PineSplitterPanel, PineSplitterResizeHandle};
pub use switch::PineSwitch;
pub use tabs::{PineTabsContent, PineTabsList, PineTabsRoot, PineTabsTrigger};
pub use tags_input::{
    PineTagsInputClear, PineTagsInputInput, PineTagsInputItem, PineTagsInputItemDelete,
    PineTagsInputItemText, PineTagsInputRoot,
};
pub use text::PineText;
pub use textarea::PineTextarea;
pub use time_field::PineTimeField;
pub use time_range_field::PineTimeRangeField;
pub use toggle::PineToggle;
pub use toggle_group::{PineToggleGroupItem, PineToggleGroupRoot};
pub use toolbar::{PineToolbarButton, PineToolbarLink, PineToolbarRoot, PineToolbarSeparator};
pub use tooltip::{
    PineTooltipContent, PineTooltipPortal, PineTooltipProvider, PineTooltipRoot, PineTooltipTrigger,
};
pub use tree::{PineTreeItem, PineTreeItemToggle, PineTreeRoot};
pub use upload::PineUpload;

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
    PineCalendarRoot::register();
    PineCalendarHeader::register();
    PineCalendarHeading::register();
    PineCalendarPrev::register();
    PineCalendarNext::register();
    PineCalendarGrid::register();
    PineCalendarGridHead::register();
    PineCalendarGridBody::register();
    PineCalendarGridRow::register();
    PineCalendarHeadCell::register();
    PineCalendarCell::register();
    PineCalendarCellTrigger::register();
    PineDatePicker::register();
    PineRangeCalendarRoot::register();
    PineRangeCalendarHeader::register();
    PineRangeCalendarHeading::register();
    PineRangeCalendarPrev::register();
    PineRangeCalendarNext::register();
    PineRangeCalendarGrid::register();
    PineRangeCalendarGridHead::register();
    PineRangeCalendarGridBody::register();
    PineRangeCalendarCell::register();
    PineRangeCalendarCellTrigger::register();
    PineDateRangePicker::register();
    PineDateField::register();
    PineTimeField::register();
    PineDateRangeField::register();
    PineTimeRangeField::register();
    PineCollapsibleRoot::register();
    PineCollapsibleTrigger::register();
    PineCollapsibleContent::register();
    PineContextMenuRoot::register();
    PineContextMenuTrigger::register();
    PineContextMenuPortal::register();
    PineContextMenuContent::register();
    PineContextMenuItem::register();
    PineContextMenuSeparator::register();
    // RFC 060 Tier 3 — bundle marker. `PineDialog::register()`
    // transitively registers all eight Dialog parts.
    PineDialog::register();
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
    PineFieldRoot::register();
    PineFieldLabel::register();
    PineFieldControl::register();
    PineFieldDescription::register();
    PineFieldError::register();
    PineFieldsetRoot::register();
    PineFieldsetLegend::register();
    PineFormRoot::register();
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
    PineInput::register();
    PineTextarea::register();
    PineOtpField::register();
    PineSliderRoot::register();
    PineSliderTrack::register();
    PineSliderRange::register();
    PineSliderThumb::register();
    PineSelectRoot::register();
    PineSelectTrigger::register();
    PineSelectValue::register();
    PineSelectPortal::register();
    PineSelectContent::register();
    PineSelectItem::register();
    PineSelectItemIndicator::register();
    PineSelectSeparator::register();
    PineComboboxRoot::register();
    PineComboboxInput::register();
    PineComboboxPortal::register();
    PineComboboxContent::register();
    PineComboboxEmpty::register();
    PineComboboxItem::register();
    PineCommandRoot::register();
    PineCommandPortal::register();
    PineCommandOverlay::register();
    PineCommandContent::register();
    PineCommandInput::register();
    PineCommandList::register();
    PineCommandItem::register();
    PineCommandEmpty::register();
    PineScrollAreaRoot::register();
    PineScrollAreaViewport::register();
    PineScrollAreaScrollbar::register();
    PineScrollAreaThumb::register();
    PineScrollAreaCorner::register();
    PineSplitterGroup::register();
    PineSplitterPanel::register();
    PineSplitterResizeHandle::register();
    PineTreeRoot::register();
    PineTreeItem::register();
    PineTreeItemToggle::register();
    PineTagsInputRoot::register();
    PineTagsInputItem::register();
    PineTagsInputItemText::register();
    PineTagsInputItemDelete::register();
    PineTagsInputInput::register();
    PineTagsInputClear::register();
    PineText::register();
    PineUpload::register();
}
