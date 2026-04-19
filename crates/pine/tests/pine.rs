//! Pine browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine`.

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

// ─── helpers ──────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount(host_html: &str) -> Element {
    pine::register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    pocopine_core::start(&host);
    host
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

// ─── PineButton ───────────────────────────────────────────────────

/// Variants and size props flow onto `data-*` attributes; the
/// native `disabled` attribute lands on the inner `<button>`.
#[wasm_bindgen_test]
async fn button_renders_data_attrs_and_disabled() {
    let host = mount(
        "<pine-button variant=\"primary\" size=\"sm\" disabled=\"true\">Save</pine-button>",
    );
    tick().await;

    let btn = host.query_selector("button.pine-btn").unwrap().unwrap();
    assert_eq!(
        btn.get_attribute("data-variant").as_deref(),
        Some("primary"),
        "variant → data-variant"
    );
    assert_eq!(btn.get_attribute("data-size").as_deref(), Some("sm"));
    assert!(
        btn.has_attribute("data-disabled"),
        "boolean disabled prop renders as data-disabled present-or-absent"
    );
    assert!(
        btn.has_attribute("disabled"),
        "disabled prop writes the native disabled attribute"
    );
    assert_eq!(
        btn.get_attribute("type").as_deref(),
        Some("button"),
        "default button type is 'button' (safe for non-form usage)"
    );

    host.remove();
}

/// `pp-as` on the component tag replaces the template's `<button>`
/// with the author's single child element — merging class attrs.
#[wasm_bindgen_test]
async fn button_pp_as_hoists_author_element() {
    let host = mount(
        "<pine-button pp-as variant=\"ghost\"><a href=\"#\" class=\"mine\">Docs</a></pine-button>",
    );
    tick().await;

    let tag = host.query_selector("pine-button").unwrap().unwrap();
    let children = tag.children();
    assert_eq!(children.length(), 1, "tag has exactly one child");
    let root = children.item(0).unwrap();
    assert_eq!(root.local_name(), "a", "hoisted to <a>");

    let cls = root.get_attribute("class").unwrap_or_default();
    assert!(cls.split_whitespace().any(|c| c == "mine"));
    assert!(
        cls.split_whitespace().any(|c| c == "pine-btn"),
        "template class merged onto <a>"
    );
    assert_eq!(root.get_attribute("data-variant").as_deref(), Some("ghost"));

    host.remove();
}

// ─── PineTabs ─────────────────────────────────────────────────────

// Host component that supplies `tabs` / `current` to the
// pine-tabs tag and updates `current` from the component's
// pp:update:model event.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template = "TabsHost.html")]
struct TabsHost {
    tabs: Vec<pine::TabDef>,
    current: String,
}

#[handlers]
impl TabsHost {
    pub fn on_mount(&mut self) {
        self.tabs = vec![
            pine::TabDef {
                value: "a".into(),
                label: "A".into(),
                disabled: false,
            },
            pine::TabDef {
                value: "b".into(),
                label: "B".into(),
                disabled: false,
            },
            pine::TabDef {
                value: "c".into(),
                label: "C".into(),
                disabled: true,
            },
        ];
        self.current = "a".into();
    }
    pub fn on_change(&mut self, ev: web_sys::CustomEvent) {
        if let Some(v) = ev.detail().as_string() {
            self.current = v;
        }
    }
}

/// Tabs render from the `tabs` prop, aria-selected reflects the
/// current `value`, clicking a tab updates `value` and fires
/// `pp:update:model`.
#[wasm_bindgen_test]
async fn tabs_render_set_aria_selected_and_emit_update_model() {
    TabsHost::register();
    let host = mount("<tabs-host></tabs-host>");
    tick().await;
    tick().await;

    let buttons = host.query_selector_all("button[role=\"tab\"]").unwrap();
    assert_eq!(buttons.length(), 3, "three tab buttons rendered");

    let a = buttons.item(0).unwrap().dyn_into::<Element>().unwrap();
    let b = buttons.item(1).unwrap().dyn_into::<Element>().unwrap();
    let c = buttons.item(2).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(a.get_attribute("aria-selected").as_deref(), Some("true"));
    assert_eq!(b.get_attribute("aria-selected").as_deref(), Some("false"));
    assert!(c.has_attribute("disabled"), "disabled tab renders disabled attr");

    // Click `B`.
    b.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    // Host's `current` moved to `b` via the emitted pp:update:model.
    let current = host.query_selector(".th-current").unwrap().unwrap();
    let html: HtmlElement = current.dyn_into().unwrap();
    assert_eq!(
        html.inner_text().trim(),
        "b",
        "host's current updated via pp:update:model"
    );

    host.remove();
}

// ─── PineTooltip ──────────────────────────────────────────────────

/// Focusing the trigger shows the tooltip immediately (no delay
/// for keyboard users per WAI-ARIA); blurring hides it.
#[wasm_bindgen_test]
async fn tooltip_shows_on_focus_and_hides_on_blur() {
    let host = mount(
        "<button id=\"tt-trig\" class=\"trig\">hover me</button>\
         <pine-tooltip trigger=\"#tt-trig\">Helpful tip.</pine-tooltip>",
    );
    tick().await;
    tick().await;

    let trigger = host
        .query_selector("#tt-trig")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();

    // No tooltip visible yet.
    assert!(
        doc()
            .query_selector("[role=\"tooltip\"].pine-tooltip")
            .unwrap()
            .is_none(),
        "tooltip starts hidden"
    );

    // Focus → shows (no delay).
    trigger.focus().unwrap();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"tooltip\"].pine-tooltip")
            .unwrap()
            .is_some(),
        "tooltip visible after focus"
    );

    // Blur → hides.
    trigger.blur().unwrap();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"tooltip\"].pine-tooltip")
            .unwrap()
            .is_none(),
        "tooltip gone after blur"
    );

    host.remove();
}

// ─── PineSwitch ───────────────────────────────────────────────────

#[wasm_bindgen_test]
async fn switch_toggles_aria_and_emits_model_event() {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount("<pine-switch checked=\"false\"></pine-switch>");
    tick().await;

    let tag = host.query_selector("pine-switch").unwrap().unwrap();
    let btn = host
        .query_selector("button[role=\"switch\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        btn.get_attribute("aria-checked").as_deref(),
        Some("false"),
        "initial aria-checked"
    );
    assert_eq!(btn.get_attribute("data-state").as_deref(), Some("unchecked"));

    let last = Rc::new(Cell::new(None::<bool>));
    let lc = last.clone();
    let cb: Closure<dyn FnMut(web_sys::CustomEvent)> =
        Closure::wrap(Box::new(move |ev: web_sys::CustomEvent| {
            lc.set(ev.detail().as_bool());
        }));
    let target: &web_sys::EventTarget = tag.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    btn.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(last.take(), Some(true), "pp:update:model fired with true");
    assert_eq!(btn.get_attribute("aria-checked").as_deref(), Some("true"));
    assert_eq!(btn.get_attribute("data-state").as_deref(), Some("checked"));

    cb.forget();
    host.remove();
}

// ─── PineCheckbox ─────────────────────────────────────────────────

#[wasm_bindgen_test]
async fn checkbox_tri_state_maps_aria_checked_correctly() {
    let host = mount("<pine-checkbox state=\"indeterminate\"></pine-checkbox>");
    tick().await;

    let btn = host
        .query_selector("button[role=\"checkbox\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        btn.get_attribute("aria-checked").as_deref(),
        Some("mixed"),
        "indeterminate maps to aria-checked=mixed"
    );
    assert_eq!(
        btn.get_attribute("data-state").as_deref(),
        Some("indeterminate")
    );

    // Click: indeterminate → checked (not → unchecked).
    btn.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(btn.get_attribute("aria-checked").as_deref(), Some("true"));
    assert_eq!(btn.get_attribute("data-state").as_deref(), Some("checked"));

    // Click again: checked → unchecked.
    btn.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(btn.get_attribute("aria-checked").as_deref(), Some("false"));
    assert_eq!(btn.get_attribute("data-state").as_deref(), Some("unchecked"));

    host.remove();
}

// ─── PineDropdownMenu — compound (Radix-style) ────────────────────

// Outer-scope host used by the compound-menu regression test
// below. The menu nests inside this component's template so its
// RFC-027 inject chain has to cross the slot-materialisation
// boundary correctly — matching the demo's layout, not the
// degenerate "menu at the root of the mount" shape.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template = "MenuHost.html")]
struct MenuHost {
    bumps: u32,
}

#[handlers]
impl MenuHost {
    pub fn bump(&mut self) {
        self.bumps += 1;
    }
}

/// Opening a menu via the trigger teleports Content to body, sets
/// the first menuitem's tabindex=0, auto-focuses it, cycles on
/// arrow keys, and closes on Escape. Exercises the full compound
/// chain: Root (state) → Trigger (toggle) → Portal (pp-if +
/// teleport) → Content (anchor + roving + escape) → Item.
#[wasm_bindgen_test]
async fn compound_menu_opens_via_trigger_cycles_and_closes_on_escape() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger>open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content anchor=\"[data-pine-dm-trigger]\">\
               <pine-dropdown-menu-item class=\"m-a\">A</pine-dropdown-menu-item>\
               <pine-dropdown-menu-item class=\"m-b\">B</pine-dropdown-menu-item>\
               <pine-dropdown-menu-item class=\"m-c\">C</pine-dropdown-menu-item>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    // Three ticks: initial render, on_ready (mirrors Root.open),
    // and the pp-if commit after Trigger clicks.
    tick().await;

    // Trigger renders a <button> inside the custom tag.
    let trigger = host
        .query_selector("pine-dropdown-menu-trigger button")
        .unwrap()
        .expect("trigger button rendered");
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false"),
        "aria-expanded mirrors Root.open = false"
    );
    assert_eq!(
        trigger.get_attribute("data-pine-dm-trigger").as_deref(),
        Some(""),
        "trigger stamped so Content's anchor selector resolves"
    );

    // Click the trigger → Root.open = true → Portal's mirror
    // effect fires → pp-if flips → Content teleports to <body>.
    let trigger_html: HtmlElement = trigger.clone().dyn_into().unwrap();
    trigger_html.click();
    tick().await;
    tick().await;

    let menu = doc()
        .query_selector("ul[role=\"menu\"].pine-dm-content")
        .unwrap()
        .expect("menu teleported to body after trigger click");
    let a = menu.query_selector(".m-a").unwrap().unwrap();
    let b = menu.query_selector(".m-b").unwrap().unwrap();

    // Content's on_ready initialised roving tabindex.
    let a_li = a.query_selector("li").unwrap().unwrap_or(a.clone());
    let b_li = b.query_selector("li").unwrap().unwrap_or(b.clone());
    assert_eq!(
        a_li.get_attribute("tabindex").as_deref(),
        Some("0"),
        "first item tabindex=0"
    );
    // auto_focus_first moved real focus to A's menuitem.
    assert_eq!(doc().active_element().unwrap(), a_li, "first item focused");

    // ArrowDown → B's menuitem.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("ArrowDown");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    a_li.dispatch_event(&ev).unwrap();
    tick().await;
    assert_eq!(doc().active_element().unwrap(), b_li, "ArrowDown → B");

    // Escape routes through Content.close() → Root.close() → pp-if flips off.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    menu.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_none(),
        "menu gone after Escape"
    );
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false"),
        "aria-expanded mirrors back to closed"
    );

    host.remove();
}

/// Regression: the compound menu is usually nested inside a user
/// component (PineDemoApp in the demo, MenuHost here), not mounted
/// bare at the document root. That changes the slot-materialisation
/// path — Trigger / Portal / Content get a non-null caller scope as
/// their DOM-borrowed scope, so the RFC-027 inject chain has to
/// walk to Root via the slot *owner*, not the caller. This test
/// fails the chain if that plumbing regresses.
#[wasm_bindgen_test]
async fn compound_menu_injects_through_slot_owner_when_nested() {
    MenuHost::register();
    let host = mount("<menu-host></menu-host>");
    tick().await;

    let trigger = host
        .query_selector(".mh-trigger button")
        .unwrap()
        .expect("trigger button rendered");

    // Open via trigger click — if inject failed, Root.toggle never
    // runs and the menu never teleports.
    trigger.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    let menu_el = doc()
        .query_selector("ul[role=\"menu\"].pine-dm-content")
        .unwrap()
        .expect("menu opened via trigger when nested inside an outer scope");

    // pp-anchor should have positioned the menu: `position: fixed`
    // + a non-empty `top` + `left`. Without this, the menu lands at
    // (0, 0) from browser defaults — exactly the "at the bottom of
    // the page, not relative to the trigger" failure mode.
    let menu_style = menu_el
        .clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .style();
    assert_eq!(
        menu_style.get_property_value("position").unwrap_or_default(),
        "fixed",
        "pp-anchor applied position: fixed"
    );
    assert!(
        !menu_style
            .get_property_value("top")
            .unwrap_or_default()
            .is_empty(),
        "pp-anchor wrote a `top` — menu is anchored, not dumped at the page root"
    );
    assert!(
        !menu_style
            .get_property_value("left")
            .unwrap_or_default()
            .is_empty(),
        "pp-anchor wrote a `left` — menu is anchored to the trigger"
    );

    // Click the first item's rendered `<li>` — clicking the inner
    // element bubbles through both Item's own `@click="on_select"`
    // (installed on the li) and the author's `@click="bump"` on
    // the outer tag. Dispatching on the tag directly would skip
    // the inner li listener entirely.
    let item_li = doc()
        .query_selector(".mh-item-a .pine-dm-item, .mh-item-a li")
        .unwrap()
        .expect("item li rendered");
    item_li.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert_eq!(
        host.query_selector(".mh-bumps")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap_or_default(),
        "1",
        "author's @click on Item ran via native bubble"
    );
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_none(),
        "menu dismissed by Item.on_select → Root.close"
    );

    host.remove();
}

// ─── PinePopover ──────────────────────────────────────────────────

/// Popover opens on `open=true`, anchors to the trigger via a
/// CSS selector prop, teleports to `<body>`, and closes on
/// Escape (restoring focus to the trigger).
#[wasm_bindgen_test]
async fn popover_opens_anchors_and_closes_on_escape() {
    let host = mount(
        "<div><button id=\"trigger-popover\" class=\"trig\">open</button>\
         <pine-popover open=\"true\" anchor=\"#trigger-popover\">\
           <button class=\"popover-btn\">OK</button>\
         </pine-popover></div>",
    );
    // Two ticks: pp-if / pp-teleport + activate's tick::next.
    tick().await;
    tick().await;

    let popover = doc()
        .query_selector("[role=\"dialog\"].pine-popover-content")
        .unwrap()
        .expect("popover teleported to body");
    // pp-anchor sets position: fixed on the floater.
    assert_eq!(
        popover
            .dyn_into::<HtmlElement>()
            .unwrap()
            .style()
            .get_property_value("position")
            .unwrap(),
        "fixed",
        "pp-anchor applied position: fixed"
    );

    // Escape closes.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    let popover_el = doc()
        .query_selector("[role=\"dialog\"].pine-popover-content")
        .unwrap()
        .unwrap();
    popover_el.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;

    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-popover-content")
            .unwrap()
            .is_none(),
        "popover gone after Escape"
    );

    host.remove();
}

// ─── PineDialog (pp-model:open round-trip) ────────────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template = "DialogHost.html")]
struct DialogHost {
    dialog_open: bool,
}

#[handlers]
impl DialogHost {
    pub fn open_it(&mut self) {
        self.dialog_open = true;
    }
}

/// pp-model:open="dialog_open" flows the parent's field into the
/// dialog's `open` prop; Escape inside the dialog fires
/// pp:update:model which writes back to the parent, so the parent's
/// state reflects the internal close.
#[wasm_bindgen_test]
async fn dialog_pp_model_open_round_trips_through_parent() {
    DialogHost::register();
    let host = mount("<dialog-host></dialog-host>");
    tick().await;

    let state_text = |host: &Element| -> String {
        host.query_selector(".dh-state")
            .unwrap()
            .unwrap()
            .dyn_into::<HtmlElement>()
            .unwrap()
            .inner_text()
            .trim()
            .to_string()
    };
    assert_eq!(state_text(&host), "closed");

    // Click "open" on the parent — sets dialog_open=true, which
    // flows through pp-model:open into the child.
    let open_btn = host
        .query_selector(".dh-open")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    open_btn.click();
    tick().await;
    tick().await;

    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_some(),
        "dialog mounted after parent set open=true"
    );
    assert_eq!(state_text(&host), "open");

    // Escape on the dialog closes it. The child fires
    // pp:update:model with false, which pp-model writes back to
    // the parent's `dialog_open`.
    let dialog = doc()
        .query_selector("[role=\"dialog\"].pine-dialog-content")
        .unwrap()
        .unwrap();
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    dialog.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;

    assert_eq!(
        state_text(&host),
        "closed",
        "parent's dialog_open flipped back to false via pp-model"
    );
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_none(),
        "dialog torn down after close"
    );

    host.remove();
}

// ─── PineDialog ───────────────────────────────────────────────────

/// Mounting a dialog with `open=true` teleports the content into
/// `<body>` with the right ARIA wiring, locks body scroll, and
/// moves focus inside. Escape closes it, restoring focus and
/// unlocking scroll.
#[wasm_bindgen_test]
async fn dialog_teleports_traps_focus_and_locks_scroll() {
    use pocopine::scroll_lock;

    // Host with a trigger button we can use as the pre-open
    // focus target.
    let host = mount(
        "<div><button class=\"trigger\">open</button>\
         <pine-dialog open=\"true\">\
           <template pp-slot=\"title\">Hi</template>\
           <template pp-slot=\"description\">Body</template>\
           <button class=\"inner-1\">A</button>\
           <button class=\"inner-2\">B</button>\
         </pine-dialog></div>",
    );
    tick().await;
    // Two ticks — first for pp-if/pp-teleport commit, second for
    // the tick::next-deferred activate() inside PineDialog::on_mount.
    tick().await;

    // Dialog content ended up in <body>, NOT inside `host`.
    let dialog = doc()
        .query_selector("[role=\"dialog\"].pine-dialog-content")
        .unwrap()
        .expect("dialog role element rendered");
    assert!(
        dialog
            .parent_element()
            .map(|p| p.local_name() == "div")
            .unwrap_or(false),
        "teleported into body's overlay wrapper, not inner pine-dialog tag"
    );

    // ARIA wiring lines up: aria-labelledby and aria-describedby
    // point at the rendered <h2>/<p>.
    let labelledby = dialog.get_attribute("aria-labelledby").unwrap_or_default();
    let title = doc().get_element_by_id(&labelledby).expect("title id resolves");
    assert!(title.inner_html().contains("Hi"));

    // Scroll lock held.
    assert!(scroll_lock::depth() >= 1, "scroll lock engaged");

    // Focus landed on the first focusable inside the dialog.
    let active = doc().active_element().unwrap();
    assert_eq!(
        active.get_attribute("class").as_deref(),
        Some("inner-1"),
        "auto_focus_first moved focus to first button inside content"
    );

    // Escape closes. Dispatch a keydown on the dialog.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    dialog.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;

    // Dialog gone from document.
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_none(),
        "dialog removed after Escape"
    );
    assert_eq!(scroll_lock::depth(), 0, "scroll lock released on close");

    host.remove();
}

/// Clicks on the inner `<button>` bubble up through the
/// `<pine-button>` custom element tag — so `@click` (or any
/// directly-attached listener) on the tag catches them. This is
/// what lets authors write `<pine-button @click="save">` without
/// any prop-drilling.
#[wasm_bindgen_test]
async fn button_clicks_bubble_through_pine_button_tag() {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount("<pine-button>Hit me</pine-button>");
    tick().await;

    let tag = host.query_selector("pine-button").unwrap().unwrap();
    let inner = host.query_selector("button.pine-btn").unwrap().unwrap();

    let fired = Rc::new(Cell::new(0u32));
    let f = fired.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> =
        Closure::wrap(Box::new(move |_| f.set(f.get() + 1)));
    let target: &web_sys::EventTarget = tag.as_ref();
    target
        .add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .unwrap();

    inner.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(
        fired.get(),
        1,
        "click on inner <button> bubbled to the <pine-button> tag"
    );

    host.remove();
}
