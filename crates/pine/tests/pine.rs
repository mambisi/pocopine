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

// ─── PineTabs (compound) ──────────────────────────────────────────

/// Compound Tabs: clicking a Trigger flips Root.value;
/// sibling Trigger/Content pairs mirror `selected` reactively.
/// Aria-selected + data-state follow, Content's pp-show gates
/// on the match, aria-labelledby points at the matching Trigger.
#[wasm_bindgen_test]
async fn tabs_compound_select_via_trigger_mirrors_siblings() {
    let host = mount(
        "<pine-tabs-root value=\"a\">\
           <pine-tabs-list>\
             <pine-tabs-trigger value=\"a\" class=\"tc-a\">A</pine-tabs-trigger>\
             <pine-tabs-trigger value=\"b\" class=\"tc-b\">B</pine-tabs-trigger>\
           </pine-tabs-list>\
           <pine-tabs-content value=\"a\" class=\"tc-panel-a\">Panel A</pine-tabs-content>\
           <pine-tabs-content value=\"b\" class=\"tc-panel-b\">Panel B</pine-tabs-content>\
         </pine-tabs-root>",
    );
    tick().await;
    tick().await;

    let trig_a = host.query_selector(".tc-a button").unwrap().unwrap();
    let trig_b = host.query_selector(".tc-b button").unwrap().unwrap();
    let panel_a: HtmlElement = host
        .query_selector(".tc-panel-a div")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    let panel_b: HtmlElement = host
        .query_selector(".tc-panel-b div")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    // Initial: A selected.
    assert_eq!(trig_a.get_attribute("aria-selected").as_deref(), Some("true"));
    assert_eq!(trig_b.get_attribute("aria-selected").as_deref(), Some("false"));
    assert_ne!(
        panel_a.style().get_property_value("display").unwrap_or_default(),
        "none",
        "panel A visible initially"
    );
    assert_eq!(
        panel_b.style().get_property_value("display").unwrap_or_default(),
        "none",
        "panel B hidden initially"
    );

    // aria-labelledby on panels points at their sibling trigger's id.
    let panel_b_el = host.query_selector(".tc-panel-b div").unwrap().unwrap();
    let labelledby = panel_b_el.get_attribute("aria-labelledby").unwrap_or_default();
    let trig_b_id = trig_b.get_attribute("id").unwrap_or_default();
    assert_eq!(labelledby, trig_b_id, "panel B aria-labelledby → trigger B id");

    // Click B → B selected; A mirrors back to inactive.
    trig_b.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert_eq!(trig_a.get_attribute("aria-selected").as_deref(), Some("false"));
    assert_eq!(trig_b.get_attribute("aria-selected").as_deref(), Some("true"));
    assert_ne!(
        panel_b.style().get_property_value("display").unwrap_or_default(),
        "none"
    );
    assert_eq!(
        panel_a.style().get_property_value("display").unwrap_or_default(),
        "none"
    );

    host.remove();
}

// ─── PineTooltip ──────────────────────────────────────────────────

/// Focusing the trigger shows the tooltip immediately (no delay
/// for keyboard users per WAI-ARIA); blurring hides it.
#[wasm_bindgen_test]
async fn tooltip_compound_shows_on_focus_and_hides_on_blur() {
    let host = mount(
        "<pine-tooltip-root>\
           <pine-tooltip-trigger><button id=\"tt-c-trig\">hover me</button></pine-tooltip-trigger>\
           <pine-tooltip-portal>\
             <pine-tooltip-content>Helpful tip.</pine-tooltip-content>\
           </pine-tooltip-portal>\
         </pine-tooltip-root>",
    );
    tick().await;
    tick().await;

    let trigger = host
        .query_selector("#tt-c-trig")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();

    // No tooltip visible yet.
    assert!(
        doc()
            .query_selector("[role=\"tooltip\"].pine-tooltip-content")
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
            .query_selector("[role=\"tooltip\"].pine-tooltip-content")
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
            .query_selector("[role=\"tooltip\"].pine-tooltip-content")
            .unwrap()
            .is_none(),
        "tooltip hidden after blur"
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
             <pine-dropdown-menu-content>\
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
    assert!(
        trigger
            .get_attribute("data-pine-dm-trigger")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        "trigger stamped with its root's scope id so Content's anchor selector resolves"
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

/// Item dispatches a cancelable `pp:select` CustomEvent. A
/// listener that calls `preventDefault()` vetoes the auto-close;
/// matches reka-ui's preventable DropdownMenuItem.select.
#[wasm_bindgen_test]
async fn dropdown_menu_item_pp_select_preventable_keeps_menu_open() {
    use wasm_bindgen::closure::Closure;

    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"pv-trig\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content>\
               <pine-dropdown-menu-item class=\"pv-keep\">Keep open</pine-dropdown-menu-item>\
               <pine-dropdown-menu-item class=\"pv-close\">Normal close</pine-dropdown-menu-item>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;

    let trigger = host.query_selector(".pv-trig button").unwrap().unwrap();
    trigger.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    // Attach a plain JS `pp:select` listener that vetoes only
    // for the "keep open" item. Stands in for what an author
    // would write with `@pp:select.prevent` or an event handler.
    let keep_li = doc()
        .query_selector(".pv-keep .pine-dm-item")
        .unwrap()
        .expect("keep-open item rendered");
    let prevent_cb: Closure<dyn FnMut(web_sys::Event)> =
        Closure::wrap(Box::new(|ev: web_sys::Event| ev.prevent_default()));
    keep_li
        .add_event_listener_with_callback("pp:select", prevent_cb.as_ref().unchecked_ref())
        .unwrap();
    prevent_cb.forget();

    // Click the vetoing item — menu should stay open.
    keep_li
        .clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_some(),
        "menu stays open when a pp:select listener calls preventDefault"
    );

    // Click the plain item — no listener → menu dismisses.
    let close_li = doc()
        .query_selector(".pv-close .pine-dm-item")
        .unwrap()
        .expect("close item rendered");
    close_li.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_none(),
        "menu dismisses when nothing prevents pp:select"
    );

    host.remove();
}

/// DropdownMenu RadioGroup + RadioItem exclusive selection:
/// clicking a RadioItem updates the group's `value`, flips
/// `aria-checked` on the clicked item to `"true"` and the
/// previously-selected one to `"false"`, and nested
/// ItemIndicators mirror accordingly.
#[wasm_bindgen_test]
async fn dropdown_menu_radio_group_exclusive_selection() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"rg-trig\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content>\
               <pine-dropdown-menu-radio-group value=\"a\">\
                 <pine-dropdown-menu-radio-item class=\"rg-a\" value=\"a\">\
                   <pine-dropdown-menu-item-indicator>●</pine-dropdown-menu-item-indicator>\
                   A\
                 </pine-dropdown-menu-radio-item>\
                 <pine-dropdown-menu-radio-item class=\"rg-b\" value=\"b\">\
                   <pine-dropdown-menu-item-indicator>●</pine-dropdown-menu-item-indicator>\
                   B\
                 </pine-dropdown-menu-radio-item>\
               </pine-dropdown-menu-radio-group>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;

    host.query_selector(".rg-trig button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    // Initial: A is selected (group.value=\"a\"). aria-checked
    // reflects that — and the indicator inside A is visible.
    let a_li = doc().query_selector(".rg-a li").unwrap().unwrap();
    let b_li = doc().query_selector(".rg-b li").unwrap().unwrap();
    assert_eq!(
        a_li.get_attribute("aria-checked").as_deref(),
        Some("true"),
        "A starts selected"
    );
    assert_eq!(
        b_li.get_attribute("aria-checked").as_deref(),
        Some("false"),
        "B starts unselected"
    );

    // Veto menu dismissal so we can keep asserting on the
    // teleported DOM after the click.
    use wasm_bindgen::closure::Closure;
    let veto: Closure<dyn FnMut(web_sys::Event)> =
        Closure::wrap(Box::new(|ev: web_sys::Event| ev.prevent_default()));
    b_li
        .add_event_listener_with_callback("pp:select", veto.as_ref().unchecked_ref())
        .unwrap();
    veto.forget();

    // Click B — group.value flips to "b", both items' aria-checked
    // mirrors update reactively via the watch_scope_field install
    // in RadioItem.on_ready.
    b_li.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert_eq!(
        a_li.get_attribute("aria-checked").as_deref(),
        Some("false"),
        "A deselected after clicking B"
    );
    assert_eq!(
        b_li.get_attribute("aria-checked").as_deref(),
        Some("true"),
        "B now selected"
    );

    host.remove();
}

/// DropdownMenu CheckboxItem + ItemIndicator round-trip: clicking
/// the item toggles its tri-state, emits pp:update:model, and the
/// nested ItemIndicator reactively renders via pp-if on the
/// mirrored `checked` bool. Matches reka-ui's CheckboxItem +
/// ItemIndicator pairing.
#[wasm_bindgen_test]
async fn dropdown_menu_checkbox_item_toggles_and_indicator_mirrors() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"ck-trig\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content>\
               <pine-dropdown-menu-checkbox-item class=\"ck-one\">\
                 <pine-dropdown-menu-item-indicator>\
                   <span class=\"ck-dot\">✓</span>\
                 </pine-dropdown-menu-item-indicator>\
                 One\
               </pine-dropdown-menu-checkbox-item>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;

    // Open the menu.
    host.query_selector(".ck-trig button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    // Initial: unchecked — aria-checked=false, indicator absent
    // from DOM (pp-if gated on `checked = false`).
    let item_li = doc()
        .query_selector(".ck-one li")
        .unwrap()
        .expect("checkbox item li");
    assert_eq!(
        item_li.get_attribute("aria-checked").as_deref(),
        Some("false"),
        "initial aria-checked"
    );
    assert!(
        doc().query_selector(".ck-dot").unwrap().is_none(),
        "indicator unmounted when unchecked (pp-if false)"
    );

    // Click — toggles to "checked", menu stays open for demo
    // purposes is *not* what we do here (default behaviour is
    // to dismiss on select, matching Item), so veto it first.
    use wasm_bindgen::closure::Closure;
    let veto: Closure<dyn FnMut(web_sys::Event)> =
        Closure::wrap(Box::new(|ev: web_sys::Event| ev.prevent_default()));
    item_li
        .add_event_listener_with_callback("pp:select", veto.as_ref().unchecked_ref())
        .unwrap();
    veto.forget();

    item_li.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    // Menu still open (veto worked), item now checked, indicator
    // rendered.
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_some(),
        "menu still open after veto"
    );
    assert_eq!(
        item_li.get_attribute("aria-checked").as_deref(),
        Some("true"),
        "aria-checked flipped on click"
    );
    assert_eq!(
        item_li.get_attribute("data-state").as_deref(),
        Some("checked"),
        "data-state reflects the tri-state value"
    );
    // pp-if now mounts the indicator + materialises its slot
    // against the right scope — so the user's `.ck-dot` span
    // actually renders in the DOM.
    assert!(
        doc().query_selector(".ck-dot").unwrap().is_some(),
        "indicator mounted + slot materialised when checked"
    );

    host.remove();
}

/// DropdownMenu's visual-only sub-parts — Separator, Group, Label —
/// render correct ARIA wiring. Separator has `role="separator"`
/// and `aria-orientation="horizontal"`. Group + Label link via
/// `aria-labelledby` → `id` with a unique per-Group id so multiple
/// groups don't collide.
#[wasm_bindgen_test]
async fn dropdown_menu_group_label_separator_wire_aria() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"gl-trig\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content>\
               <pine-dropdown-menu-group class=\"gl-group\">\
                 <pine-dropdown-menu-label>Actions</pine-dropdown-menu-label>\
                 <pine-dropdown-menu-item class=\"gl-a\">A</pine-dropdown-menu-item>\
               </pine-dropdown-menu-group>\
               <pine-dropdown-menu-separator class=\"gl-sep\"></pine-dropdown-menu-separator>\
               <pine-dropdown-menu-item class=\"gl-b\">B</pine-dropdown-menu-item>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;

    // Click trigger to open.
    let trigger = host.query_selector(".gl-trig button").unwrap().unwrap();
    trigger.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    // Separator: role + orientation on the inner li.
    let sep = doc()
        .query_selector(".gl-sep li")
        .unwrap()
        .expect("separator li rendered");
    assert_eq!(sep.get_attribute("role").as_deref(), Some("separator"));
    assert_eq!(
        sep.get_attribute("aria-orientation").as_deref(),
        Some("horizontal")
    );

    // Group + Label: group's aria-labelledby points at the
    // rendered Label's id; the id is non-empty + instance-unique
    // (derived from the Group's scope id).
    let group_root = doc()
        .query_selector(".gl-group div[role=\"group\"]")
        .unwrap()
        .expect("group role element");
    let label_el = doc()
        .query_selector(".pine-dm-label")
        .unwrap()
        .expect("label rendered");
    let label_id = label_el.get_attribute("id").unwrap_or_default();
    assert!(!label_id.is_empty(), "label has id");
    assert!(
        label_id.starts_with("pine-dm-group-label-"),
        "label id namespaced"
    );
    assert_eq!(
        group_root.get_attribute("aria-labelledby").as_deref(),
        Some(label_id.as_str()),
        "group aria-labelledby → label id"
    );

    host.remove();
}

/// Two DropdownMenus side-by-side must each anchor to their own
/// Trigger — not all share-the-first via a common selector. The
/// `on_setup` hook (runs pre-children-walk) computes Content's
/// `anchor` selector from the injected root's scope id, so every
/// menu instance's `pp-anchor` resolves to a distinct trigger
/// button.
#[wasm_bindgen_test]
async fn two_dropdown_menus_anchor_to_their_own_triggers() {
    let host = mount(
        "<div>\
           <pine-dropdown-menu-root>\
             <pine-dropdown-menu-trigger class=\"t1\">one</pine-dropdown-menu-trigger>\
             <pine-dropdown-menu-portal>\
               <pine-dropdown-menu-content>\
                 <pine-dropdown-menu-item class=\"i1\">A</pine-dropdown-menu-item>\
               </pine-dropdown-menu-content>\
             </pine-dropdown-menu-portal>\
           </pine-dropdown-menu-root>\
           <pine-dropdown-menu-root>\
             <pine-dropdown-menu-trigger class=\"t2\">two</pine-dropdown-menu-trigger>\
             <pine-dropdown-menu-portal>\
               <pine-dropdown-menu-content>\
                 <pine-dropdown-menu-item class=\"i2\">B</pine-dropdown-menu-item>\
               </pine-dropdown-menu-content>\
             </pine-dropdown-menu-portal>\
           </pine-dropdown-menu-root>\
         </div>",
    );
    tick().await;

    // Each Trigger stamps its button with its root scope id —
    // distinct per instance. Pre-`on_setup`, the stamp was a
    // shared empty string and both menus' Content anchored to
    // the first trigger in the document.
    let b1 = host.query_selector(".t1 button").unwrap().unwrap();
    let b2 = host.query_selector(".t2 button").unwrap().unwrap();
    let id1 = b1
        .get_attribute("data-pine-dm-trigger")
        .unwrap_or_default();
    let id2 = b2
        .get_attribute("data-pine-dm-trigger")
        .unwrap_or_default();
    assert!(!id1.is_empty(), "trigger 1 stamped");
    assert!(!id2.is_empty(), "trigger 2 stamped");
    assert_ne!(id1, id2, "each menu gets its own trigger id");

    // Open menu 2 via its trigger. Only one menu should be in
    // body, and its anchor should be targeting trigger 2.
    b2.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    let menu = doc()
        .query_selector("ul[role=\"menu\"].pine-dm-content")
        .unwrap()
        .expect("menu 2 open");
    let menu_html: HtmlElement = menu.clone().dyn_into().unwrap();
    assert_eq!(
        menu_html.style().get_property_value("position").unwrap_or_default(),
        "fixed",
        "pp-anchor positioned the menu"
    );

    // Menu 1 should stay closed — no second teleported menu in body.
    let menus_open = doc()
        .query_selector_all("ul[role=\"menu\"].pine-dm-content")
        .unwrap();
    assert_eq!(menus_open.length(), 1, "exactly one menu open");

    host.remove();
}

// ─── PineAvatar ───────────────────────────────────────────────────

/// Avatar starts with Fallback visible (Root.loaded=false). When
/// the browser fires `load` on the `<img>`, Image.on_load flips
/// Root.loaded=true → Fallback's `pp-show="!loaded"` hides it.
/// Uses a tiny 1x1 `data:` URL so the load fires deterministically.
#[wasm_bindgen_test]
async fn avatar_fallback_hides_after_image_loads() {
    let host = mount(
        "<pine-avatar-root><pine-avatar-image src=\"data:image/gif;base64,R0lGODlhAQABAIAAAP///wAAACwAAAAAAQABAAACAkQBADs=\" alt=\"tiny\"></pine-avatar-image><pine-avatar-fallback><span class=\"av-fb\">AB</span></pine-avatar-fallback></pine-avatar-root>",
    );
    tick().await;

    let fallback: HtmlElement = host
        .query_selector(".pine-avatar-fallback")
        .unwrap()
        .expect("fallback root rendered")
        .dyn_into()
        .unwrap();
    assert_ne!(
        fallback.style().get_property_value("display").unwrap_or_default(),
        "none",
        "fallback visible while image not-yet-loaded"
    );

    // Wait for the browser's async image-load event — fires on
    // a macrotask, not a microtask, so plain `tick()` (a chain
    // of Promise.resolve awaits) is insufficient. Yield via a
    // real setTimeout each iteration.
    for _ in 0..20 {
        let p = js_sys::Promise::new(&mut |resolve, _| {
            let _ = window()
                .unwrap()
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    &resolve, 0,
                );
        });
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
        tick().await;
        if fallback.style().get_property_value("display").unwrap_or_default() == "none" {
            break;
        }
    }

    assert_eq!(
        fallback.style().get_property_value("display").unwrap_or_default(),
        "none",
        "fallback hidden once the image load fired"
    );

    host.remove();
}

// ─── PineAccordion ────────────────────────────────────────────────

/// Accordion type="single" + collapsible: clicking an Item's
/// Trigger opens it; clicking the open one closes it. Opening
/// one closes any other.
#[wasm_bindgen_test]
async fn accordion_single_collapsible_exclusive_toggle() {
    let host = mount(
        "<pine-accordion-root type=\"single\" collapsible=\"true\">\
           <pine-accordion-item value=\"a\">\
             <pine-accordion-trigger class=\"ac-t-a\">A</pine-accordion-trigger>\
             <pine-accordion-content><p class=\"ac-body-a\">Body A</p></pine-accordion-content>\
           </pine-accordion-item>\
           <pine-accordion-item value=\"b\">\
             <pine-accordion-trigger class=\"ac-t-b\">B</pine-accordion-trigger>\
             <pine-accordion-content><p class=\"ac-body-b\">Body B</p></pine-accordion-content>\
           </pine-accordion-item>\
         </pine-accordion-root>",
    );
    tick().await;

    let trig_a = host.query_selector(".ac-t-a button").unwrap().unwrap();
    let trig_b = host.query_selector(".ac-t-b button").unwrap().unwrap();

    // Start: nothing open.
    assert!(
        host.query_selector(".ac-body-a").unwrap().is_none(),
        "A closed initially"
    );
    assert!(
        host.query_selector(".ac-body-b").unwrap().is_none(),
        "B closed initially"
    );

    // Click A → A opens.
    trig_a
        .clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        host.query_selector(".ac-body-a").unwrap().is_some(),
        "A opens on click"
    );
    assert_eq!(
        trig_a.get_attribute("aria-expanded").as_deref(),
        Some("true")
    );

    // Click B → B opens, A closes (single mode).
    trig_b
        .clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        host.query_selector(".ac-body-b").unwrap().is_some(),
        "B opens"
    );
    assert!(
        host.query_selector(".ac-body-a").unwrap().is_none(),
        "A closed by single-mode exclusivity"
    );

    // Click B again → B closes (collapsible=true).
    trig_b.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert!(
        host.query_selector(".ac-body-b").unwrap().is_none(),
        "B closes on second click when collapsible"
    );

    host.remove();
}

// ─── PineCollapsible ──────────────────────────────────────────────

/// Collapsible's Trigger toggles Root.open; Content is gated on
/// the same value via pp-if. Validates the second compound
/// pattern end-to-end (DropdownMenu was the first) — proves the
/// substrate scales beyond menus.
#[wasm_bindgen_test]
async fn collapsible_trigger_toggles_and_content_mounts() {
    let host = mount(
        "<pine-collapsible-root>\
           <pine-collapsible-trigger class=\"cp-trig\">Toggle</pine-collapsible-trigger>\
           <pine-collapsible-content>\
             <p class=\"cp-body\">Revealed.</p>\
           </pine-collapsible-content>\
         </pine-collapsible-root>",
    );
    tick().await;

    let trigger = host.query_selector(".cp-trig button").unwrap().unwrap();

    // Initial state: closed — aria-expanded=false, content not
    // mounted (pp-if gated on Content.open=false).
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false"),
        "initial aria-expanded reflects Root.open=false"
    );
    assert!(
        host.query_selector(".cp-body").unwrap().is_none(),
        "content not in DOM when closed"
    );

    // Click → Root.open flips, Trigger's mirror fires,
    // Content's mirror fires → pp-if mounts the body.
    trigger
        .clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("true")
    );
    assert!(
        host.query_selector(".cp-body").unwrap().is_some(),
        "content mounted after open"
    );

    // Click again → closes, body unmounts.
    trigger.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert!(
        host.query_selector(".cp-body").unwrap().is_none(),
        "content unmounted after close"
    );

    host.remove();
}

// ─── PinePopover ──────────────────────────────────────────────────

/// Popover compound: clicking Trigger toggles Root.open, Portal
/// teleports, Content auto-anchors to the Trigger via the
/// `data-pine-popover-trigger` stamp, Escape closes.
#[wasm_bindgen_test]
async fn popover_opens_anchors_and_closes_on_escape() {
    let host = mount(
        "<pine-popover-root>\
           <pine-popover-trigger class=\"pt-trig\">open</pine-popover-trigger>\
           <pine-popover-portal>\
             <pine-popover-content>\
               <button class=\"popover-btn\">OK</button>\
             </pine-popover-content>\
           </pine-popover-portal>\
         </pine-popover-root>",
    );
    tick().await;

    // Click Trigger → Root.open=true → Portal mirror fires → teleport.
    let trigger = host.query_selector(".pt-trig button").unwrap().unwrap();
    trigger.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    let popover = doc()
        .query_selector("[role=\"dialog\"].pine-popover-content")
        .unwrap()
        .expect("popover teleported to body");
    // pp-anchor sets position: fixed on the floater.
    assert_eq!(
        popover
            .clone()
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
    popover.dispatch_event(&ev).unwrap();
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

/// Dialog compound: clicking Trigger opens Root, Portal teleports
/// Content + Overlay into `<body>`, Content gets role="dialog" +
/// ARIA wiring via the Root-provided title/description ids, focus
/// moves inside, scroll lock engages. Escape closes.
#[wasm_bindgen_test]
async fn dialog_teleports_traps_focus_and_locks_scroll() {
    use pocopine::scroll_lock;

    let host = mount(
        "<pine-dialog-root>\
           <pine-dialog-trigger class=\"dg-trig\">open</pine-dialog-trigger>\
           <pine-dialog-portal>\
             <pine-dialog-overlay></pine-dialog-overlay>\
             <pine-dialog-content>\
               <pine-dialog-title>Hi</pine-dialog-title>\
               <pine-dialog-description>Body</pine-dialog-description>\
               <button class=\"inner-1\">A</button>\
               <button class=\"inner-2\">B</button>\
             </pine-dialog-content>\
           </pine-dialog-portal>\
         </pine-dialog-root>",
    );
    tick().await;

    // Click Trigger → open.
    host.query_selector(".dg-trig button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    // Dialog content ended up in <body>, not inside `host`.
    let dialog = doc()
        .query_selector("[role=\"dialog\"].pine-dialog-content")
        .unwrap()
        .expect("dialog role element rendered");

    // ARIA wiring: aria-labelledby + aria-describedby point at
    // rendered Title / Description.
    let labelledby = dialog.get_attribute("aria-labelledby").unwrap_or_default();
    let title = doc()
        .get_element_by_id(&labelledby)
        .expect("title id resolves");
    assert!(title.inner_html().contains("Hi"));

    // Scroll lock engaged (modal default = true).
    assert!(scroll_lock::depth() >= 1, "scroll lock engaged");

    // Focus moved to first focusable inside the content.
    let active = doc().active_element().unwrap();
    assert_eq!(
        active.get_attribute("class").as_deref(),
        Some("inner-1"),
        "overlay::activate auto-focused first button"
    );

    // Escape closes via Content.on_escape → Root.close.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    dialog.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;

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
