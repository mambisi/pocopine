//! Static metadata for the component reference pages.
//!
//! Each Pine primitive has a [`ComponentMeta`] entry (slug, title,
//! category, blurb, and optional anatomy + props). The live preview
//! is the matching `<*-demo>` (mounted by `ComponentPage` via
//! `pp-if`), and the **Code** tab shows that demo's real `.poco`
//! source via [`code_for`] (`include_str!`) — so the example code is
//! always exactly what renders, never drifts.

/// One row of a component's props / data-attribute table.
pub struct PropRow {
    pub name: &'static str,
    pub ty: &'static str,
    pub default: &'static str,
    pub desc: &'static str,
}

/// Reference-page metadata for a single Pine component.
pub struct ComponentMeta {
    /// URL slug, e.g. `"alert-dialog"`. The live demo tag is
    /// `<{slug}-demo>` and `pp-if` keys on this value.
    pub slug: &'static str,
    pub title: &'static str,
    pub category: &'static str,
    pub blurb: &'static str,
    /// Optional tag-tree skeleton; empty string hides the section.
    pub anatomy: &'static str,
    pub props: &'static [PropRow],
}

pub const CAT_FORMS: &str = "Forms & inputs";
pub const CAT_OVERLAYS: &str = "Overlays & menus";
pub const CAT_NAV: &str = "Navigation & layout";
pub const CAT_DISPLAY: &str = "Display";
pub const CAT_DATETIME: &str = "Date & time";

/// Category display order for the index page.
pub const CATEGORY_ORDER: &[&str] = &[CAT_FORMS, CAT_OVERLAYS, CAT_NAV, CAT_DISPLAY, CAT_DATETIME];

pub const COMPONENTS: &[ComponentMeta] = &[
    // ── Forms & inputs ───────────────────────────────────────────
    ComponentMeta {
        slug: "button",
        title: "Button",
        category: CAT_FORMS,
        blurb: "A clickable action with variants and disabled state.",
        anatomy: "",
        props: &[
            PropRow { name: "variant", ty: "\"primary\" | \"destructive\" | \"ghost\"", default: "—", desc: "Visual style (omit for the default outline)." },
            PropRow { name: "disabled", ty: "bool", default: "false", desc: "Disables interaction; exposes data-disabled." },
        ],
    },
    ComponentMeta {
        slug: "input",
        title: "Input",
        category: CAT_FORMS,
        blurb: "A single-line text field that binds with pp-model.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "switch-checkbox",
        title: "Switch & Checkbox",
        category: CAT_FORMS,
        blurb: "Boolean toggles — a switch and a checkbox, both pp-model bound.",
        anatomy: "",
        props: &[
            PropRow { name: "checked", ty: "bool (pp-model)", default: "false", desc: "Two-way bound on/off state." },
            PropRow { name: "disabled", ty: "bool", default: "false", desc: "Disables interaction." },
        ],
    },
    ComponentMeta {
        slug: "radio-group",
        title: "Radio Group",
        category: CAT_FORMS,
        blurb: "A single-select set of radio options with roving focus.",
        anatomy: "<pine-radio-group-root pp-model:value>\n  <pine-radio-group-item value=\"…\" />\n</pine-radio-group-root>",
        props: &[
            PropRow { name: "value", ty: "String (pp-model)", default: "—", desc: "Selected option value." },
        ],
    },
    ComponentMeta {
        slug: "select",
        title: "Select",
        category: CAT_FORMS,
        blurb: "A listbox-style dropdown for choosing one option.",
        anatomy: "<pine-select-root pp-model:value>\n  <pine-select-trigger />\n  <pine-select-content>\n    <pine-select-item value=\"…\" />\n  </pine-select-content>\n</pine-select-root>",
        props: &[
            PropRow { name: "value", ty: "String (pp-model)", default: "—", desc: "Selected option value." },
        ],
    },
    ComponentMeta {
        slug: "combobox",
        title: "Combobox",
        category: CAT_FORMS,
        blurb: "A filterable text input paired with a popover list.",
        anatomy: "<pine-combobox-root>\n  <pine-combobox-input />\n  <pine-combobox-content>\n    <pine-combobox-item />\n    <pine-combobox-empty />\n  </pine-combobox-content>\n</pine-combobox-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "slider",
        title: "Slider",
        category: CAT_FORMS,
        blurb: "A draggable range input with keyboard support.",
        anatomy: "",
        props: &[
            PropRow { name: "value", ty: "f64 (pp-model)", default: "0", desc: "Current value." },
            PropRow { name: "min / max / step", ty: "f64", default: "0 / 100 / 1", desc: "Range bounds and increment." },
        ],
    },
    ComponentMeta {
        slug: "toggle",
        title: "Toggle",
        category: CAT_FORMS,
        blurb: "A two-state pressed/unpressed button.",
        anatomy: "",
        props: &[
            PropRow { name: "pressed", ty: "bool (pp-model)", default: "false", desc: "Pressed state; exposes data-state." },
        ],
    },
    ComponentMeta {
        slug: "field",
        title: "Field",
        category: CAT_FORMS,
        blurb: "A label + control + description/error wrapper for accessible forms.",
        anatomy: "<pine-field-root name=\"…\">\n  <pine-field-label />\n  <pine-field-control />\n  <pine-field-description />\n  <pine-field-error />\n</pine-field-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "fieldset",
        title: "Fieldset",
        category: CAT_FORMS,
        blurb: "A grouped set of related fields with a legend.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "form",
        title: "Form",
        category: CAT_FORMS,
        blurb: "A form root that wires fields, validation, and submission.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "otp",
        title: "OTP Field",
        category: CAT_FORMS,
        blurb: "A segmented one-time-passcode input.",
        anatomy: "",
        props: &[
            PropRow { name: "length", ty: "usize", default: "6", desc: "Number of code segments." },
        ],
    },
    ComponentMeta {
        slug: "tags-input",
        title: "Tags Input",
        category: CAT_FORMS,
        blurb: "An editable list of chips; reordering FLIPs on drag.",
        anatomy: "",
        props: &[],
    },
    // ── Overlays & menus ─────────────────────────────────────────
    ComponentMeta {
        slug: "dialog",
        title: "Dialog",
        category: CAT_OVERLAYS,
        blurb: "A modal dialog with focus trap and overlay.",
        anatomy: "<pine-dialog-root pp-model:open>\n  <pine-dialog-trigger />\n  <pine-dialog-portal>\n    <pine-dialog-overlay />\n    <pine-dialog-content>\n      <pine-dialog-title />\n      <pine-dialog-description />\n      <pine-dialog-close />\n    </pine-dialog-content>\n  </pine-dialog-portal>\n</pine-dialog-root>",
        props: &[
            PropRow { name: "open", ty: "bool (pp-model)", default: "false", desc: "Open state on the root." },
        ],
    },
    ComponentMeta {
        slug: "alert-dialog",
        title: "Alert Dialog",
        category: CAT_OVERLAYS,
        blurb: "A confirmation dialog with explicit action / cancel.",
        anatomy: "<pine-alert-dialog-root>\n  <pine-alert-dialog-trigger />\n  <pine-alert-dialog-portal>\n    <pine-alert-dialog-content>\n      <pine-alert-dialog-action />\n      <pine-alert-dialog-cancel />\n    </pine-alert-dialog-content>\n  </pine-alert-dialog-portal>\n</pine-alert-dialog-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "popover",
        title: "Popover",
        category: CAT_OVERLAYS,
        blurb: "Floating content anchored to a trigger.",
        anatomy: "<pine-popover-root pp-model:open>\n  <pine-popover-trigger />\n  <pine-popover-content />\n</pine-popover-root>",
        props: &[
            PropRow { name: "open", ty: "bool (pp-model)", default: "false", desc: "Open state on the root." },
        ],
    },
    ComponentMeta {
        slug: "dropdown-menu",
        title: "Dropdown Menu",
        category: CAT_OVERLAYS,
        blurb: "An actions menu with items, separators, and checkboxes.",
        anatomy: "<pine-dropdown-menu-root>\n  <pine-dropdown-menu-trigger />\n  <pine-dropdown-menu-content>\n    <pine-dropdown-menu-item />\n    <pine-dropdown-menu-separator />\n  </pine-dropdown-menu-content>\n</pine-dropdown-menu-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "context-menu",
        title: "Context Menu",
        category: CAT_OVERLAYS,
        blurb: "A right-click menu anchored at the pointer.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "hover-card",
        title: "Hover Card",
        category: CAT_OVERLAYS,
        blurb: "A preview card that opens on hover with a delay.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "tooltip",
        title: "Tooltip",
        category: CAT_OVERLAYS,
        blurb: "A small label shown on hover or focus.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "command",
        title: "Command",
        category: CAT_OVERLAYS,
        blurb: "A ⌘K command palette with search and keyboard nav.",
        anatomy: "<pine-command-root shortcut=\"Mod+k\">\n  <pine-command-portal>\n    <pine-command-content>\n      <pine-command-input />\n      <pine-command-list>\n        <pine-command-item />\n        <pine-command-empty />\n      </pine-command-list>\n    </pine-command-content>\n  </pine-command-portal>\n</pine-command-root>",
        props: &[],
    },
    // ── Navigation & layout ──────────────────────────────────────
    ComponentMeta {
        slug: "tabs",
        title: "Tabs",
        category: CAT_NAV,
        blurb: "A tabbed panel switcher with roving focus.",
        anatomy: "<pine-tabs-root pp-model:value>\n  <pine-tabs-list>\n    <pine-tabs-trigger value=\"…\" />\n  </pine-tabs-list>\n  <pine-tabs-content value=\"…\" />\n</pine-tabs-root>",
        props: &[
            PropRow { name: "value", ty: "String (pp-model)", default: "—", desc: "Active tab value." },
        ],
    },
    ComponentMeta {
        slug: "accordion",
        title: "Accordion",
        category: CAT_NAV,
        blurb: "Stacked, collapsible sections.",
        anatomy: "<pine-accordion-root>\n  <pine-accordion-item>\n    <pine-accordion-trigger />\n    <pine-accordion-content />\n  </pine-accordion-item>\n</pine-accordion-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "collapsible",
        title: "Collapsible",
        category: CAT_NAV,
        blurb: "A single expandable/collapsible region.",
        anatomy: "<pine-collapsible-root pp-model:open>\n  <pine-collapsible-trigger />\n  <pine-collapsible-content />\n</pine-collapsible-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "toolbar",
        title: "Toolbar",
        category: CAT_NAV,
        blurb: "A horizontal container of grouped controls.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "scroll-area",
        title: "Scroll Area",
        category: CAT_NAV,
        blurb: "A styled, cross-browser custom scroll container.",
        anatomy: "<pine-scroll-area-root type=\"hover\">\n  <pine-scroll-area-viewport />\n  <pine-scroll-area-scrollbar>\n    <pine-scroll-area-thumb />\n  </pine-scroll-area-scrollbar>\n</pine-scroll-area-root>",
        props: &[
            PropRow { name: "type", ty: "\"auto\" | \"always\" | \"scroll\" | \"hover\"", default: "\"hover\"", desc: "When scrollbars are shown." },
        ],
    },
    ComponentMeta {
        slug: "splitter",
        title: "Splitter",
        category: CAT_NAV,
        blurb: "Resizable panes with a draggable handle.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "tree",
        title: "Tree",
        category: CAT_NAV,
        blurb: "A nested, keyboard-navigable tree view.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "aspect-ratio",
        title: "Aspect Ratio",
        category: CAT_NAV,
        blurb: "Constrains content to a fixed width/height ratio.",
        anatomy: "",
        props: &[
            PropRow { name: "ratio", ty: "f64", default: "1.0", desc: "Width-to-height ratio, e.g. 16/9." },
        ],
    },
    // ── Display ──────────────────────────────────────────────────
    ComponentMeta {
        slug: "avatar",
        title: "Avatar",
        category: CAT_DISPLAY,
        blurb: "A user image with a fallback when it fails to load.",
        anatomy: "<pine-avatar-root>\n  <pine-avatar-image src=\"…\" />\n  <pine-avatar-fallback />\n</pine-avatar-root>",
        props: &[],
    },
    ComponentMeta {
        slug: "text",
        title: "Text",
        category: CAT_DISPLAY,
        blurb: "Typographic primitives for headings and body copy.",
        anatomy: "",
        props: &[],
    },
    // ── Date & time ──────────────────────────────────────────────
    ComponentMeta {
        slug: "calendar",
        title: "Calendar",
        category: CAT_DATETIME,
        blurb: "A month-grid date picker with keyboard nav.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "range-calendar",
        title: "Range Calendar",
        category: CAT_DATETIME,
        blurb: "A calendar for selecting a start/end date range.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "date-picker",
        title: "Date Picker",
        category: CAT_DATETIME,
        blurb: "A date field paired with a calendar popover.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "date-range-picker",
        title: "Date Range Picker",
        category: CAT_DATETIME,
        blurb: "A range field paired with a range calendar.",
        anatomy: "",
        props: &[],
    },
    ComponentMeta {
        slug: "datetime-fields",
        title: "Date & Time Fields",
        category: CAT_DATETIME,
        blurb: "Segmented date and time entry fields.",
        anatomy: "",
        props: &[],
    },
];

/// Find a component's metadata by slug.
pub fn find(slug: &str) -> Option<&'static ComponentMeta> {
    COMPONENTS.iter().find(|c| c.slug == slug)
}

// The **Code** tab source is each demo's real `.poco`, syntect-
// highlighted at build time (see `build.rs` → `gen_code::component`).
// `build.rs` scans the demo directories directly — the slug is the
// directory name with `_` → `-` — so the example shown is exactly what
// the **Preview** mounts, with no duplicated path table here.
