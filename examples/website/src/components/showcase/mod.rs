//! Pine UI showcase. Each primitive demo lives in its own
//! subfolder with a focused `#[component]` owning just the
//! state that demo needs — no shared blob on a parent.
//!
//! `Showcase.poco` itself is thin: it composes the 31 demos by
//! dropping their custom tags in, plus the section-divider +
//! intro paragraph. Same pattern the `hn` example uses.

pub mod accordion;
pub mod alert_dialog;
pub mod aspect_ratio;
pub mod avatar;
pub mod basics;
pub mod button;
pub mod cmd_popover;
pub mod collapsible;
pub mod combobox;
pub mod command;
pub mod context_menu;
pub mod dialog;
pub mod dropdown_menu;
pub mod field;
pub mod fieldset;
pub mod form;
pub mod hover_card;
pub mod input;
pub mod otp;
pub mod popover;
pub mod radio_group;
pub mod scroll_area;
pub mod select;
pub mod signup;
pub mod slider;
pub mod splitter;
pub mod stress;
pub mod switch_checkbox;
pub mod tabs;
pub mod tags_input;
pub mod tags_mentions;
pub mod tags_skills;
pub mod text;
pub mod toggle;
pub mod toolbar;
pub mod tooltip;
pub mod tree;

pub use accordion::AccordionDemo;
pub use alert_dialog::AlertDialogDemo;
pub use aspect_ratio::AspectRatioDemo;
pub use avatar::AvatarDemo;
pub use basics::Basics;
pub use button::ButtonDemo;
pub use cmd_popover::CmdPopoverDemo;
pub use collapsible::CollapsibleDemo;
pub use combobox::ComboboxDemo;
pub use command::CommandDemo;
pub use context_menu::ContextMenuDemo;
pub use dialog::DialogDemo;
pub use dropdown_menu::DropdownMenuDemo;
pub use field::FieldDemo;
pub use fieldset::FieldsetDemo;
pub use form::FormDemo;
pub use hover_card::HoverCardDemo;
pub use input::InputDemo;
pub use otp::OtpDemo;
pub use popover::PopoverDemo;
pub use radio_group::RadioGroupDemo;
pub use scroll_area::ScrollAreaDemo;
pub use select::SelectDemo;
pub use signup::SignupDemo;
pub use slider::SliderDemo;
pub use splitter::SplitterDemo;
pub use stress::StressDemo;
pub use switch_checkbox::SwitchCheckboxDemo;
pub use tabs::TabsDemo;
pub use tags_input::TagsInputDemo;
pub use tags_mentions::TagsMentionsDemo;
pub use tags_skills::TagsSkillsDemo;
pub use text::TextDemo;
pub use toggle::ToggleDemo;
pub use toolbar::ToolbarDemo;
pub use tooltip::TooltipDemo;
pub use tree::TreeDemo;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// The showcase container. Stateless — just drops every demo's
/// custom tag into `Showcase.poco`. Each demo owns its own
/// reactive state.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "Showcase.poco", role = "panel")]
pub struct Showcase {}

#[handlers]
impl Showcase {}
