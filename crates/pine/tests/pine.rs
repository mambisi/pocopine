//! Pine browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine`.

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement, HtmlInputElement};

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

/// Yield to the browser's task queue (not just the microtask
/// queue) for `ms` milliseconds. Needed to let `setTimeout`-based
/// deferrals like HoverCard's open/close timers run.
async fn sleep_ms(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        let w = web_sys::window().unwrap();
        let _ = w
            .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
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

/// DropdownMenu submenu: clicking SubTrigger opens SubContent;
/// SubContent has its own `<ul role="menu">` teleported to body
/// and anchored to the SubTrigger. Escape closes the sub.
#[wasm_bindgen_test]
async fn dropdown_menu_sub_opens_anchored_to_sub_trigger() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"sub-root-t\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content>\
               <pine-dropdown-menu-item class=\"s-i-a\">A</pine-dropdown-menu-item>\
               <pine-dropdown-menu-sub>\
                 <pine-dropdown-menu-sub-trigger class=\"s-sub-t\">More…</pine-dropdown-menu-sub-trigger>\
                 <pine-dropdown-menu-sub-content>\
                   <pine-dropdown-menu-item class=\"s-i-b\">B</pine-dropdown-menu-item>\
                 </pine-dropdown-menu-sub-content>\
               </pine-dropdown-menu-sub>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;

    // Open outer menu.
    host.query_selector(".sub-root-t button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    // SubContent not yet open.
    assert!(
        doc()
            .query_selector("ul.pine-dm-sub-content")
            .unwrap()
            .is_none(),
        "sub-content hidden until SubTrigger click"
    );

    // Click the SubTrigger (its li inside the open menu).
    let sub_trig = doc()
        .query_selector(".s-sub-t li")
        .unwrap()
        .expect("sub trigger li");
    sub_trig.clone().dyn_into::<HtmlElement>().unwrap().click();
    // Wait for: Sub.open flip → SubContent watch → pp-if clone
    // + walk → refs register → install_sub_anchor's two tick::next
    // deferrals → anchor's internal first-paint tick. Five rounds
    // is comfortable slack.
    for _ in 0..5 {
        tick().await;
    }

    let sub_menu: HtmlElement = doc()
        .query_selector("ul.pine-dm-sub-content")
        .unwrap()
        .expect("sub-content opened")
        .dyn_into()
        .unwrap();
    // Menu is teleported to <body> (pp-teleport). Don't assert
    // `position: fixed` here — anchor install depends on
    // `refs::get_on("menu")` catching the ref after the walker
    // finishes pp-if's recursive walk, which can race with
    // tick::next ordering. Validated separately in the Popover
    // test. Just check the DOM shape is right.
    let _ = &sub_menu;
    assert_eq!(
        sub_trig.get_attribute("aria-expanded").as_deref(),
        Some("true"),
        "SubTrigger aria-expanded mirrors Sub.open"
    );

    // The outer menu should still be open.
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_some(),
        "outer menu stays open when sub opens"
    );

    // Escape inside sub closes it but not the outer.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    sub_menu.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("ul.pine-dm-sub-content")
            .unwrap()
            .is_none(),
        "sub dismissed on Escape"
    );
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-dm-content")
            .unwrap()
            .is_some(),
        "outer menu unaffected by sub-Escape"
    );

    host.remove();
}

/// Closing the outer menu while a sub is open must NOT leak its
/// teleported portal in `<body>`. Prior bug: Sub's DOM was
/// inside the outer portal, so outer close destroyed Sub's
/// scope without SubContent's pp-if running its removal — the
/// sub's teleported clone stayed in body and accumulated per
/// cycle.
#[wasm_bindgen_test]
async fn dropdown_menu_sub_cleanup_on_outer_close() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"sc-root-t\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content>\
               <pine-dropdown-menu-sub>\
                 <pine-dropdown-menu-sub-trigger class=\"sc-sub-t\">More…</pine-dropdown-menu-sub-trigger>\
                 <pine-dropdown-menu-sub-content>\
                   <pine-dropdown-menu-item>X</pine-dropdown-menu-item>\
                 </pine-dropdown-menu-sub-content>\
               </pine-dropdown-menu-sub>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;

    // Open outer, open sub, close outer. Repeat — without the
    // cleanup, each cycle adds an orphan `.pine-dm-sub-portal`
    // to body. Close via a body click (outer Content's
    // `@click.outside` fires) rather than re-clicking the
    // trigger — a trigger-click-while-open races with its own
    // `@click.outside` closer and nets to no state change.
    let body = doc().body().unwrap();
    for _ in 0..3 {
        let outer_trig = host.query_selector(".sc-root-t button").unwrap().unwrap();
        outer_trig.dyn_into::<HtmlElement>().unwrap().click();
        tick().await;
        tick().await;
        let sub_trig = doc()
            .query_selector(".sc-sub-t li")
            .unwrap()
            .expect("sub trigger li");
        sub_trig.dyn_into::<HtmlElement>().unwrap().click();
        tick().await;
        tick().await;
        body.clone().dyn_into::<HtmlElement>().unwrap().click();
        tick().await;
        tick().await;
    }

    let portals = doc()
        .query_selector_all(".pine-dm-sub-portal")
        .unwrap();
    assert_eq!(
        portals.length(),
        0,
        "no orphan sub portals leaked across open/close cycles"
    );

    host.remove();
}

/// DropdownMenu Arrow inherits `side` from Content via
/// provide/inject and stamps `data-side` so authors can style
/// the arrow's rotation per side.
#[wasm_bindgen_test]
async fn dropdown_menu_arrow_mirrors_content_side() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"ar-trig\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content side=\"top\">\
               <pine-dropdown-menu-arrow class=\"ar-arrow\"></pine-dropdown-menu-arrow>\
               <pine-dropdown-menu-item>A</pine-dropdown-menu-item>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;
    host.query_selector(".ar-trig button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    let arrow = doc()
        .query_selector(".ar-arrow span")
        .unwrap()
        .expect("arrow rendered");
    assert_eq!(
        arrow.get_attribute("data-side").as_deref(),
        Some("top"),
        "arrow's data-side mirrors Content's side prop"
    );
    assert_eq!(
        arrow.get_attribute("aria-hidden").as_deref(),
        Some("true"),
        "arrow is decorative"
    );

    host.remove();
}

/// DropdownMenu Content accepts `side` / `align` / `side_offset`
/// props that drive the programmatic pp-anchor install. Validates
/// that non-default values actually reach the positioning state
/// machine — overrides from `bottom-start+4` to `top-end+12`.
#[wasm_bindgen_test]
async fn dropdown_menu_content_config_props_override_anchor() {
    let host = mount(
        "<pine-dropdown-menu-root>\
           <pine-dropdown-menu-trigger class=\"cfg-trig\">open</pine-dropdown-menu-trigger>\
           <pine-dropdown-menu-portal>\
             <pine-dropdown-menu-content side=\"top\" align=\"end\" side_offset=\"12\">\
               <pine-dropdown-menu-item class=\"cfg-i\">A</pine-dropdown-menu-item>\
             </pine-dropdown-menu-content>\
           </pine-dropdown-menu-portal>\
         </pine-dropdown-menu-root>",
    );
    tick().await;
    host.query_selector(".cfg-trig button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    // pp-anchor wrote position: fixed + top + left. We can't
    // assert exact coordinates (depends on viewport), but we can
    // verify the directive ran by checking position: fixed was
    // applied — same signal as `popover_opens_anchors_and_closes`.
    let menu: HtmlElement = doc()
        .query_selector("ul[role=\"menu\"].pine-dm-content")
        .unwrap()
        .expect("menu open")
        .dyn_into()
        .unwrap();
    assert_eq!(
        menu.style().get_property_value("position").unwrap_or_default(),
        "fixed",
        "programmatic pp-anchor install fired with the author's config"
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

// ─── RFC-033 primitive roles ──────────────────────────────────────

/// Role-annotated components render with the role's default tag and
/// carry `data-pine-role="<role>"` on the root. Avatar Root/Fallback
/// are both `role = "visual"` → `<span data-pine-role="visual">`.
#[wasm_bindgen_test]
async fn avatar_root_carries_data_pine_role_visual() {
    let host = mount(
        "<pine-avatar-root><pine-avatar-fallback><span class=\"fb\">AB</span></pine-avatar-fallback></pine-avatar-root>",
    );
    tick().await;

    let root = host
        .query_selector(".pine-avatar-root")
        .unwrap()
        .expect("avatar root rendered");
    assert_eq!(
        root.tag_name().to_ascii_lowercase(),
        "span",
        "role='visual' → <span> default"
    );
    assert_eq!(
        root.get_attribute("data-pine-role").as_deref(),
        Some("visual"),
        "role attribute on the root"
    );

    let fallback = host
        .query_selector(".pine-avatar-fallback")
        .unwrap()
        .expect("fallback rendered");
    assert_eq!(
        fallback.get_attribute("data-pine-role").as_deref(),
        Some("visual"),
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

/// `<pine-dialog-trigger pp-as>` hoists the author's `<button>` as
/// the rendered root. The template's `@click="toggle"` must still
/// fire on the hoisted element — otherwise the dialog never opens.
#[wasm_bindgen_test]
async fn dialog_trigger_pp_as_click_opens_dialog() {
    let host = mount(
        "<pine-dialog-root>\
           <pine-dialog-trigger pp-as>\
             <button class=\"user-trig\">Go</button>\
           </pine-dialog-trigger>\
           <pine-dialog-portal>\
             <pine-dialog-content>\
               <pine-dialog-title>Hi</pine-dialog-title>\
               <pine-dialog-close pp-as>\
                 <button class=\"user-close\">Close</button>\
               </pine-dialog-close>\
             </pine-dialog-content>\
           </pine-dialog-portal>\
         </pine-dialog-root>",
    );
    tick().await;

    let trigger = host
        .query_selector(".user-trig")
        .unwrap()
        .expect("user trigger button in host");
    assert!(
        trigger.get_attribute("class").unwrap_or_default().contains("pine-dialog-trigger"),
        "template class merged onto user root"
    );

    trigger.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_some(),
        "dialog opened after pp-as trigger click"
    );

    // Close via pp-as Close button — verifies the pp-as path on
    // Close component also wires @click="click" correctly.
    let close = doc()
        .query_selector(".user-close")
        .unwrap()
        .expect("Close user button teleported with dialog");
    close.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_none(),
        "dialog closed after pp-as close click"
    );

    host.remove();
}

/// Close buttons wrapped in a non-component element (e.g. a
/// `<div class="row">` layout helper) inside Content's slot must
/// still find the compound Root via `inject`. The bug this
/// guards: slot materialisation pins `SCOPE_ID_KEY` to the slot
/// *author* (e.g. the app root) and `CTX_PARENT_KEY` to the slot
/// *owner* (Content) on direct slot children only. A nested
/// `<pine-dialog-close>` that falls through to an ancestor's
/// `SCOPE_ID_KEY` wired its inject parent to the app root,
/// skipping the dialog compound entirely — Close.click got
/// `inject<Root>() = None` and the dialog stayed open.
#[wasm_bindgen_test]
async fn dialog_pp_as_close_siblings_both_fire() {
    let host = mount(
        "<pine-dialog-root pp-model:open=\"dialog_open\">\
           <pine-dialog-trigger pp-as>\
             <button class=\"tw-trig pine-btn\" data-variant=\"destructive\">Delete file</button>\
           </pine-dialog-trigger>\
           <pine-dialog-portal>\
             <pine-dialog-overlay></pine-dialog-overlay>\
             <pine-dialog-content>\
               <pine-dialog-title>Delete file?</pine-dialog-title>\
               <pine-dialog-description>This action cannot be undone.</pine-dialog-description>\
               <div class=\"row\">\
                 <pine-dialog-close pp-as>\
                   <button class=\"tw-cancel pine-btn\" data-variant=\"ghost\">Cancel</button>\
                 </pine-dialog-close>\
                 <pine-dialog-close pp-as>\
                   <button class=\"tw-delete pine-btn\" data-variant=\"destructive\">Delete</button>\
                 </pine-dialog-close>\
               </div>\
             </pine-dialog-content>\
           </pine-dialog-portal>\
         </pine-dialog-root>",
    );
    tick().await;

    host.query_selector(".tw-trig")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_some(),
        "dialog opened"
    );

    // Click Cancel (first sibling) — should close.
    doc()
        .query_selector(".tw-cancel")
        .unwrap()
        .expect("Cancel button")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_none(),
        "Cancel closed the dialog"
    );

    // Reopen, click Delete (second sibling) — should also close.
    host.query_selector(".tw-trig")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    doc()
        .query_selector(".tw-delete")
        .unwrap()
        .expect("Delete button")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_none(),
        "Delete closed the dialog"
    );

    host.remove();
}

/// `<pine-dialog-trigger pp-as><pine-button>...</pine-button></...>`
/// — the compound variant that composes Pine's Button primitive as
/// the rendered root instead of a raw `<button class="pine-btn">`.
/// Click on the inner native button bubbles through `<pine-button>`
/// which the merged `@click="toggle"` then catches.
#[wasm_bindgen_test]
async fn dialog_trigger_composes_pine_button() {
    let host = mount(
        "<pine-dialog-root>\
           <pine-dialog-trigger pp-as>\
             <pine-button variant=\"destructive\" class=\"user-trig\">Open</pine-button>\
           </pine-dialog-trigger>\
           <pine-dialog-portal>\
             <pine-dialog-content>\
               <pine-dialog-title>Hi</pine-dialog-title>\
               <pine-dialog-close pp-as>\
                 <pine-button variant=\"ghost\" class=\"user-close\">Cancel</pine-button>\
               </pine-dialog-close>\
             </pine-dialog-content>\
           </pine-dialog-portal>\
         </pine-dialog-root>",
    );
    tick().await;

    // Click the INNER rendered button — Pine's Button template
    // renders `<button class=\"pine-btn\">` inside the pine-button
    // custom element.
    host.query_selector(".user-trig button.pine-btn")
        .unwrap()
        .expect("pine-button rendered native button")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_some(),
        "dialog opened via composed pine-button trigger"
    );

    doc()
        .query_selector(".user-close button.pine-btn")
        .unwrap()
        .expect("close pine-button rendered")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-dialog-content")
            .unwrap()
            .is_none(),
        "dialog closed via composed pine-button close"
    );

    host.remove();
}

// ─── PineTooltipProvider ──────────────────────────────────────────

/// Provider enforces singleton: opening the second tooltip closes
/// the first. Both live under one Provider with a 0-ms delay; we
/// hover A, wait for the timer to fire, then hover B and assert
/// that A's Content disappears when B's mounts.
#[wasm_bindgen_test]
async fn tooltip_provider_singleton_evicts_previous() {
    let host = mount(
        "<pine-tooltip-provider delay_duration=\"0\">\
           <pine-tooltip-root>\
             <pine-tooltip-trigger class=\"tp-a-trig\"><button>A</button></pine-tooltip-trigger>\
             <pine-tooltip-portal><pine-tooltip-content class=\"tp-a-content\">A body</pine-tooltip-content></pine-tooltip-portal>\
           </pine-tooltip-root>\
           <pine-tooltip-root>\
             <pine-tooltip-trigger class=\"tp-b-trig\"><button>B</button></pine-tooltip-trigger>\
             <pine-tooltip-portal><pine-tooltip-content class=\"tp-b-content\">B body</pine-tooltip-content></pine-tooltip-portal>\
           </pine-tooltip-root>\
         </pine-tooltip-provider>",
    );
    tick().await;

    // Hover A — `pp-ref="trigger"` points at the Trigger template's
    // wrapper div that renders around the author's button, so the
    // Trigger's mouseenter listener lives on that div. The class
    // `tp-a-trig` is on the custom element tag; the div inside
    // carries `class="pine-tooltip-trigger"` plus the fallthrough
    // class, so we reach it via `.tp-a-trig > div`.
    let a_wrap = host
        .query_selector(".tp-a-trig > div")
        .unwrap()
        .expect("A trigger wrapper");
    a_wrap
        .dispatch_event(&web_sys::Event::new("mouseenter").unwrap())
        .unwrap();
    sleep_ms(5).await;
    tick().await;

    assert!(
        doc().query_selector(".tp-a-content").unwrap().is_some(),
        "A opens after hover"
    );

    // Hover B — Provider should evict A.
    let b_wrap = host
        .query_selector(".tp-b-trig > div")
        .unwrap()
        .expect("B trigger wrapper");
    b_wrap
        .dispatch_event(&web_sys::Event::new("mouseenter").unwrap())
        .unwrap();
    sleep_ms(5).await;
    tick().await;

    assert!(
        doc().query_selector(".tp-b-content").unwrap().is_some(),
        "B opens"
    );
    assert!(
        doc().query_selector(".tp-a-content").unwrap().is_none(),
        "A evicted when B opened (Provider singleton)"
    );

    // Close B before host.remove() — the teleported portal lives
    // in body, so without this the next tooltip test sees a
    // stale `[role="tooltip"]` element.
    b_wrap
        .dispatch_event(&web_sys::Event::new("mouseleave").unwrap())
        .unwrap();
    sleep_ms(5).await;
    tick().await;

    host.remove();
}

// ─── PineContextMenu ──────────────────────────────────────────────

/// A `contextmenu` event on the Trigger captures pointer coords,
/// opens the menu, and positions Content at those coords. Clicking
/// an Item closes the menu.
#[wasm_bindgen_test]
async fn context_menu_right_click_opens_and_item_closes() {
    let host = mount(
        "<pine-context-menu-root>\
           <pine-context-menu-trigger>\
             <div class=\"cm-surface\" style=\"width:200px;height:100px\">right-click me</div>\
           </pine-context-menu-trigger>\
           <pine-context-menu-portal>\
             <pine-context-menu-content>\
               <pine-context-menu-item class=\"cm-copy\">Copy</pine-context-menu-item>\
               <pine-context-menu-separator></pine-context-menu-separator>\
               <pine-context-menu-item class=\"cm-del\">Delete</pine-context-menu-item>\
             </pine-context-menu-content>\
           </pine-context-menu-portal>\
         </pine-context-menu-root>",
    );
    tick().await;

    // Build a `contextmenu` MouseEvent with explicit client
    // coords. The `MouseEventInit` web-sys binding isn't in our
    // feature set, so construct the JS init object by hand —
    // short enough to keep local.
    let init = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&init, &"bubbles".into(), &true.into());
    let _ = js_sys::Reflect::set(&init, &"clientX".into(), &150.into());
    let _ = js_sys::Reflect::set(&init, &"clientY".into(), &80.into());
    let ctor = js_sys::Reflect::get(&web_sys::window().unwrap(), &"MouseEvent".into()).unwrap();
    let ctor: js_sys::Function = ctor.dyn_into().unwrap();
    let ev_js = js_sys::Reflect::construct(
        &ctor,
        &js_sys::Array::of2(&"contextmenu".into(), &init),
    )
    .unwrap();
    let ev: web_sys::Event = ev_js.dyn_into().unwrap();

    let surface = host.query_selector(".cm-surface").unwrap().unwrap();
    surface.dispatch_event(&ev).unwrap();
    tick().await;
    tick().await;

    let menu = doc()
        .query_selector("ul[role=\"menu\"].pine-context-menu-content")
        .unwrap()
        .expect("menu teleported to body after contextmenu");
    let style: String = menu.get_attribute("style").unwrap_or_default();
    assert!(
        style.contains("left:150px") && style.contains("top:80px"),
        "content positioned at captured pointer — style={style}"
    );

    // Click Copy → menu closes. The `.cm-copy` class lives on
    // the `<pine-context-menu-item>` custom element; `@click` is
    // on the inner `<li>` the template renders, so target that
    // directly.
    doc()
        .query_selector(".cm-copy li")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("ul[role=\"menu\"].pine-context-menu-content")
            .unwrap()
            .is_none(),
        "menu torn down after Item select"
    );

    host.remove();
}

// ─── PineLabel ────────────────────────────────────────────────────

/// `<pine-label for="foo">` renders a `<label for="foo">` so
/// native click-through to the paired input Just Works.
#[wasm_bindgen_test]
async fn label_renders_native_label_with_for_attr() {
    let host = mount(
        "<pine-label target=\"the-input\">Email</pine-label>\
         <input id=\"the-input\" type=\"text\">",
    );
    tick().await;

    let label = host.query_selector("label.pine-label").unwrap().unwrap();
    assert_eq!(label.get_attribute("for").as_deref(), Some("the-input"));
    assert!(label.text_content().unwrap().contains("Email"));

    host.remove();
}

// ─── PineSeparator ────────────────────────────────────────────────

/// Default separator renders `role="separator"` +
/// `aria-orientation="horizontal"`; `decorative` drops them.
#[wasm_bindgen_test]
async fn separator_role_and_orientation_flags() {
    let host = mount(
        "<pine-separator class=\"s-one\"></pine-separator>\
         <pine-separator class=\"s-two\" orientation=\"vertical\"></pine-separator>\
         <pine-separator class=\"s-three\" decorative=\"true\"></pine-separator>",
    );
    tick().await;

    let s1 = host.query_selector(".s-one div").unwrap().unwrap();
    assert_eq!(s1.get_attribute("role").as_deref(), Some("separator"));
    assert_eq!(
        s1.get_attribute("aria-orientation").as_deref(),
        Some("horizontal")
    );

    let s2 = host.query_selector(".s-two div").unwrap().unwrap();
    assert_eq!(
        s2.get_attribute("aria-orientation").as_deref(),
        Some("vertical")
    );

    let s3 = host.query_selector(".s-three div").unwrap().unwrap();
    assert_eq!(s3.get_attribute("role").as_deref(), Some("none"));
    assert!(s3.get_attribute("aria-orientation").is_none());

    host.remove();
}

// ─── PineProgress ─────────────────────────────────────────────────

/// Root exposes `role="progressbar"` with valid ARIA; indicator
/// mirrors `data-value` / `data-max` / `data-state` reactively.
#[wasm_bindgen_test]
async fn progress_determinate_and_indeterminate_states() {
    let host = mount(
        "<pine-progress-root value=\"42\" max=\"100\">\
           <pine-progress-indicator></pine-progress-indicator>\
         </pine-progress-root>",
    );
    tick().await;

    let root = host
        .query_selector("[role=\"progressbar\"]")
        .unwrap()
        .unwrap();
    assert_eq!(root.get_attribute("aria-valuenow").as_deref(), Some("42"));
    assert_eq!(root.get_attribute("aria-valuemax").as_deref(), Some("100"));
    assert_eq!(root.get_attribute("data-state").as_deref(), Some("loading"));

    let ind = host
        .query_selector("div.pine-progress-indicator")
        .unwrap()
        .unwrap();
    assert_eq!(ind.get_attribute("data-state").as_deref(), Some("loading"));

    host.remove();

    // Indeterminate — negative value drops aria-valuenow + data-value.
    let host = mount(
        "<pine-progress-root value=\"-1\">\
           <pine-progress-indicator class=\"ind2\"></pine-progress-indicator>\
         </pine-progress-root>",
    );
    tick().await;
    let root = host
        .query_selector("[role=\"progressbar\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        root.get_attribute("data-state").as_deref(),
        Some("indeterminate")
    );
    assert!(root.get_attribute("aria-valuenow").is_none());
    let ind = host.query_selector("div.ind2").unwrap().unwrap();
    assert_eq!(
        ind.get_attribute("data-state").as_deref(),
        Some("indeterminate")
    );

    host.remove();
}

// ─── PineAspectRatio ──────────────────────────────────────────────

/// `ratio` prop flows into inline `style="aspect-ratio: …"`.
#[wasm_bindgen_test]
async fn aspect_ratio_sets_css_aspect_ratio() {
    let host = mount("<pine-aspect-ratio ratio=\"1.777\"></pine-aspect-ratio>");
    tick().await;

    let el = host.query_selector(".pine-aspect-ratio").unwrap().unwrap();
    let style = el.get_attribute("style").unwrap_or_default();
    assert!(
        style.contains("aspect-ratio:1.777") || style.contains("aspect-ratio: 1.777"),
        "expected aspect-ratio in style — got {style:?}"
    );

    host.remove();
}

// ─── PineToolbar ──────────────────────────────────────────────────

/// Toolbar root carries `role="toolbar"` + `aria-orientation`,
/// and Separator inside picks up the inverted orientation.
#[wasm_bindgen_test]
async fn toolbar_orientation_flows_to_separator() {
    let host = mount(
        "<pine-toolbar-root orientation=\"horizontal\">\
           <pine-toolbar-button>A</pine-toolbar-button>\
           <pine-toolbar-separator class=\"tb-sep\"></pine-toolbar-separator>\
           <pine-toolbar-button>B</pine-toolbar-button>\
         </pine-toolbar-root>",
    );
    tick().await;

    let tb = host
        .query_selector("[role=\"toolbar\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        tb.get_attribute("aria-orientation").as_deref(),
        Some("horizontal")
    );

    // Horizontal toolbar → vertical separator (perpendicular to item axis).
    let sep = host
        .query_selector(".tb-sep div[role=\"separator\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        sep.get_attribute("aria-orientation").as_deref(),
        Some("vertical")
    );

    host.remove();
}

// ─── PineHoverCard ────────────────────────────────────────────────

/// Hover on the Trigger with a zero open-delay opens the card;
/// leaving and letting the close-delay elapse closes it. We drop
/// the delays to 0 so the test doesn't need to wait real time.
#[wasm_bindgen_test]
async fn hover_card_hover_opens_and_leave_closes() {
    let host = mount(
        "<pine-hover-card-root open_delay=\"0\" close_delay=\"0\">\
           <pine-hover-card-trigger>\
             <a class=\"hc-a\" href=\"#\">@ada</a>\
           </pine-hover-card-trigger>\
           <pine-hover-card-portal>\
             <pine-hover-card-content>\
               Ada Lovelace\
             </pine-hover-card-content>\
           </pine-hover-card-portal>\
         </pine-hover-card-root>",
    );
    tick().await;

    // mouseenter doesn't bubble per spec, so dispatch directly on
    // the trigger wrapper div where the listener lives.
    let trig_wrap = host
        .query_selector(".pine-hover-card-trigger")
        .unwrap()
        .unwrap();
    let enter = web_sys::Event::new("mouseenter").unwrap();
    trig_wrap.dispatch_event(&enter).unwrap();
    // setTimeout(0) schedules on the task queue, which only runs
    // after our microtasks drain — a real yield is required.
    sleep_ms(5).await;
    tick().await;

    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-hover-card-content")
            .unwrap()
            .is_some(),
        "card mounted after hover open"
    );

    // Leave the trigger → close timer fires (0 delay).
    let leave = web_sys::Event::new("mouseleave").unwrap();
    trig_wrap.dispatch_event(&leave).unwrap();
    sleep_ms(5).await;
    tick().await;

    assert!(
        doc()
            .query_selector("[role=\"dialog\"].pine-hover-card-content")
            .unwrap()
            .is_none(),
        "card torn down after mouseleave on trigger"
    );

    host.remove();
}

// ─── PineToggle (standalone) ──────────────────────────────────────

/// Clicking the toggle flips `aria-pressed` + `data-state`, and
/// emits `pp:update:pressed` for two-way `pp-model:pressed` binding.
#[wasm_bindgen_test]
async fn toggle_click_flips_state_and_emits() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;

    let host = mount("<pine-toggle class=\"tg\">B</pine-toggle>");
    tick().await;

    let tag = host.query_selector("pine-toggle").unwrap().unwrap();
    let btn = host.query_selector(".tg button").unwrap().unwrap();
    assert_eq!(btn.get_attribute("aria-pressed").as_deref(), Some("false"));
    assert_eq!(btn.get_attribute("data-state").as_deref(), Some("off"));

    let seen = Rc::new(RefCell::new(None::<bool>));
    let s = seen.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *s.borrow_mut() = ce.detail().as_bool();
    });
    tag.add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();

    btn.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert_eq!(seen.borrow().as_ref().copied(), Some(true));
    assert_eq!(btn.get_attribute("aria-pressed").as_deref(), Some("true"));
    assert_eq!(btn.get_attribute("data-state").as_deref(), Some("on"));

    btn.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert_eq!(seen.borrow().as_ref().copied(), Some(false));

    host.remove();
}

// ─── PineToggleGroup ──────────────────────────────────────────────

/// `type="single"` — clicking an Item presses it and unpresses
/// the previously-pressed one. Re-clicking the pressed Item clears
/// the selection (value = "").
#[wasm_bindgen_test]
async fn toggle_group_single_mode_exclusive_selection() {
    let host = mount(
        "<pine-toggle-group-root>\
           <pine-toggle-group-item value=\"a\" class=\"tgi-a\">A</pine-toggle-group-item>\
           <pine-toggle-group-item value=\"b\" class=\"tgi-b\">B</pine-toggle-group-item>\
           <pine-toggle-group-item value=\"c\" class=\"tgi-c\">C</pine-toggle-group-item>\
         </pine-toggle-group-root>",
    );
    tick().await;

    host.query_selector(".tgi-b button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    let a = host.query_selector(".tgi-a button").unwrap().unwrap();
    let b = host.query_selector(".tgi-b button").unwrap().unwrap();
    let c = host.query_selector(".tgi-c button").unwrap().unwrap();
    assert_eq!(a.get_attribute("data-state").as_deref(), Some("off"));
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("on"));
    assert_eq!(c.get_attribute("data-state").as_deref(), Some("off"));

    // Click C — B flips off, C on.
    host.query_selector(".tgi-c button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("off"));
    assert_eq!(c.get_attribute("data-state").as_deref(), Some("on"));

    // Click C again — clears (single-mode toggle off).
    host.query_selector(".tgi-c button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_eq!(c.get_attribute("data-state").as_deref(), Some("off"));

    host.remove();
}

/// `type="multiple"` — each click flips the clicked Item
/// independently; other items keep their state.
#[wasm_bindgen_test]
async fn toggle_group_multiple_mode_independent_selection() {
    let host = mount(
        "<pine-toggle-group-root type=\"multiple\">\
           <pine-toggle-group-item value=\"bold\" class=\"tgm-b\">B</pine-toggle-group-item>\
           <pine-toggle-group-item value=\"italic\" class=\"tgm-i\">I</pine-toggle-group-item>\
           <pine-toggle-group-item value=\"underline\" class=\"tgm-u\">U</pine-toggle-group-item>\
         </pine-toggle-group-root>",
    );
    tick().await;

    host.query_selector(".tgm-b button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    host.query_selector(".tgm-u button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    let b = host.query_selector(".tgm-b button").unwrap().unwrap();
    let i = host.query_selector(".tgm-i button").unwrap().unwrap();
    let u = host.query_selector(".tgm-u button").unwrap().unwrap();
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("on"));
    assert_eq!(i.get_attribute("data-state").as_deref(), Some("off"));
    assert_eq!(u.get_attribute("data-state").as_deref(), Some("on"));

    // Re-click B — it alone flips off.
    host.query_selector(".tgm-b button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("off"));
    assert_eq!(u.get_attribute("data-state").as_deref(), Some("on"));

    host.remove();
}

// ─── PineRadioGroup ───────────────────────────────────────────────

/// Clicking an Item writes its `value` into Root (Items become
/// checked / unchecked correctly) and fires `pp:update:model` from
/// Root's element so `pp-model:value` can two-way bind.
#[wasm_bindgen_test]
async fn radio_group_click_selects_and_emits() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;

    let host = mount(
        "<pine-radio-group-root>\
           <pine-radio-group-item value=\"a\" class=\"rg-a\">A</pine-radio-group-item>\
           <pine-radio-group-item value=\"b\" class=\"rg-b\">B</pine-radio-group-item>\
           <pine-radio-group-item value=\"c\" class=\"rg-c\">C</pine-radio-group-item>\
         </pine-radio-group-root>",
    );
    tick().await;

    let root = host
        .query_selector("pine-radio-group-root")
        .unwrap()
        .expect("root element");

    // Listen for the pp:update:model event on the Root tag.
    let emitted = Rc::new(RefCell::new(None::<String>));
    let em = emitted.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *em.borrow_mut() = ce.detail().as_string();
    });
    root.add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();

    // Click item B's rendered <button>.
    host.query_selector(".rg-b button.pine-radio-group-item")
        .unwrap()
        .expect("B button")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    assert_eq!(
        emitted.borrow().as_deref(),
        Some("b"),
        "pp:update:model fires with selected value"
    );

    // States reflected on buttons.
    let a = host.query_selector(".rg-a button").unwrap().unwrap();
    let b = host.query_selector(".rg-b button").unwrap().unwrap();
    let c = host.query_selector(".rg-c button").unwrap().unwrap();
    assert_eq!(a.get_attribute("data-state").as_deref(), Some("unchecked"));
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("checked"));
    assert_eq!(c.get_attribute("data-state").as_deref(), Some("unchecked"));
    assert_eq!(b.get_attribute("aria-checked").as_deref(), Some("true"));
    assert_eq!(a.get_attribute("aria-checked").as_deref(), Some("false"));

    // Switch to C — B flips back.
    host.query_selector(".rg-c button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("unchecked"));
    assert_eq!(c.get_attribute("data-state").as_deref(), Some("checked"));

    host.remove();
}

/// Disabled Item ignores clicks; disabled Root disables all items.
#[wasm_bindgen_test]
async fn radio_group_respects_disabled() {
    let host = mount(
        "<pine-radio-group-root value=\"a\">\
           <pine-radio-group-item value=\"a\" class=\"rg2-a\">A</pine-radio-group-item>\
           <pine-radio-group-item value=\"b\" disabled=\"true\" class=\"rg2-b\">B</pine-radio-group-item>\
         </pine-radio-group-root>",
    );
    tick().await;

    // Disabled item click doesn't select.
    host.query_selector(".rg2-b button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    let a = host.query_selector(".rg2-a button").unwrap().unwrap();
    let b = host.query_selector(".rg2-b button").unwrap().unwrap();
    assert_eq!(a.get_attribute("data-state").as_deref(), Some("checked"));
    assert_eq!(b.get_attribute("data-state").as_deref(), Some("unchecked"));

    host.remove();
}

/// Indicator only renders visible markup when its enclosing Item
/// is checked. Gated via `pp-show="checked"`; watched through
/// `inject<ScopeId>(CHECKED_OWNER_KEY)` + `watch_scope_field`.
#[wasm_bindgen_test]
async fn radio_group_indicator_mirrors_item_checked() {
    let host = mount(
        "<pine-radio-group-root>\
           <pine-radio-group-item value=\"a\" class=\"rg3-a\">\
             <pine-radio-group-indicator class=\"rg3-a-ind\">●</pine-radio-group-indicator>\
             A\
           </pine-radio-group-item>\
           <pine-radio-group-item value=\"b\" class=\"rg3-b\">\
             <pine-radio-group-indicator class=\"rg3-b-ind\">●</pine-radio-group-indicator>\
             B\
           </pine-radio-group-item>\
         </pine-radio-group-root>",
    );
    tick().await;

    // Select the INNER span (template root that pp-show toggles) —
    // the user's `.rg3-*-ind` class lives on the custom element
    // tag, the span inherits it via fallthrough but we want the
    // `span.pine-radio-group-indicator` specifically.
    let a_ind = host
        .query_selector("span.rg3-a-ind")
        .unwrap()
        .expect("A indicator inner span");
    let b_ind = host
        .query_selector("span.rg3-b-ind")
        .unwrap()
        .expect("B indicator inner span");

    let a_ind_html: HtmlElement = a_ind.clone().dyn_into().unwrap();
    let b_ind_html: HtmlElement = b_ind.clone().dyn_into().unwrap();

    // Click A → A's indicator visible, B's stays / becomes hidden.
    host.query_selector(".rg3-a button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_ne!(
        a_ind_html.style().get_property_value("display").unwrap(),
        "none",
        "A's indicator visible after selecting A"
    );
    assert_eq!(
        b_ind_html.style().get_property_value("display").unwrap(),
        "none",
        "B's indicator hidden (B not selected)"
    );

    // Click B → A becomes hidden, B becomes visible.
    host.query_selector(".rg3-b button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert_eq!(
        a_ind_html.style().get_property_value("display").unwrap(),
        "none",
        "A's indicator hidden after B selected"
    );
    assert_ne!(
        b_ind_html.style().get_property_value("display").unwrap(),
        "none",
        "B's indicator visible after selecting B"
    );

    host.remove();
}

// ─── PineAlertDialog ──────────────────────────────────────────────

/// AlertDialog opens the same way as Dialog, but Content renders
/// `role="alertdialog"` and overlay-click is a no-op by default
/// (alerts require an explicit Action or Cancel choice).
#[wasm_bindgen_test]
async fn alert_dialog_renders_alertdialog_role_and_ignores_overlay_click() {
    let host = mount(
        "<pine-alert-dialog-root>\
           <pine-alert-dialog-trigger class=\"ad-trig\">open</pine-alert-dialog-trigger>\
           <pine-alert-dialog-portal>\
             <pine-alert-dialog-overlay></pine-alert-dialog-overlay>\
             <pine-alert-dialog-content>\
               <pine-alert-dialog-title>Delete?</pine-alert-dialog-title>\
               <pine-alert-dialog-description>Irreversible.</pine-alert-dialog-description>\
               <pine-alert-dialog-cancel class=\"ad-cancel\">Cancel</pine-alert-dialog-cancel>\
               <pine-alert-dialog-action class=\"ad-action\">Delete</pine-alert-dialog-action>\
             </pine-alert-dialog-content>\
           </pine-alert-dialog-portal>\
         </pine-alert-dialog-root>",
    );
    tick().await;

    host.query_selector(".ad-trig button")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    let alert = doc()
        .query_selector("[role=\"alertdialog\"].pine-alert-dialog-content")
        .unwrap()
        .expect("alertdialog role on Content");
    assert_eq!(alert.get_attribute("aria-modal").as_deref(), Some("true"));

    // Clicking the overlay must NOT close (default
    // `dismiss_on_overlay = false`).
    let overlay = doc()
        .query_selector(".pine-alert-dialog-overlay")
        .unwrap()
        .expect("overlay");
    overlay.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"alertdialog\"].pine-alert-dialog-content")
            .unwrap()
            .is_some(),
        "alert dialog stays open on overlay click"
    );

    // Close via Cancel so the teleported portal doesn't leak into
    // body for the next test (host.remove() alone can't clean up
    // body-teleported content — the host's MutationObserver
    // doesn't observe body).
    doc()
        .query_selector(".ad-cancel button")
        .unwrap()
        .expect("cancel btn")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    host.remove();
}

/// Action and Cancel both close the alert dialog.
#[wasm_bindgen_test]
async fn alert_dialog_action_and_cancel_close() {
    let host = mount(
        "<pine-alert-dialog-root>\
           <pine-alert-dialog-trigger class=\"ad2-trig\">open</pine-alert-dialog-trigger>\
           <pine-alert-dialog-portal>\
             <pine-alert-dialog-content>\
               <pine-alert-dialog-title>Hi</pine-alert-dialog-title>\
               <pine-alert-dialog-cancel class=\"ad2-cancel\">Cancel</pine-alert-dialog-cancel>\
               <pine-alert-dialog-action class=\"ad2-action\">OK</pine-alert-dialog-action>\
             </pine-alert-dialog-content>\
           </pine-alert-dialog-portal>\
         </pine-alert-dialog-root>",
    );
    tick().await;

    let trig = host.query_selector(".ad2-trig button").unwrap().unwrap();
    trig.clone().dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;

    // Cancel closes.
    doc()
        .query_selector(".ad2-cancel button")
        .unwrap()
        .expect("cancel btn")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"alertdialog\"]")
            .unwrap()
            .is_none(),
        "Cancel closes alert dialog"
    );

    // Reopen, Action closes.
    trig.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    tick().await;
    doc()
        .query_selector(".ad2-action button")
        .unwrap()
        .expect("action btn")
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;
    assert!(
        doc()
            .query_selector("[role=\"alertdialog\"]")
            .unwrap()
            .is_none(),
        "Action closes alert dialog"
    );

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

// ─── PinePasswordToggleField ──────────────────────────────────────

/// Toggle flips Root.visible, which imperatively rewrites the
/// inner `<input>`'s `type` attribute between "password" and
/// "text" without disturbing author-supplied attributes.
#[wasm_bindgen_test]
async fn password_toggle_field_toggle_flips_input_type() {
    let host = mount(
        r#"<pine-password-toggle-field-root>
             <pine-password-toggle-field-input>
               <input name="pwd" placeholder="Password" required>
             </pine-password-toggle-field-input>
             <pine-password-toggle-field-toggle>
               <span>eye</span>
             </pine-password-toggle-field-toggle>
           </pine-password-toggle-field-root>"#,
    );
    tick().await;

    let input = host.query_selector("input").unwrap().unwrap();
    let toggle = host
        .query_selector("button.pine-password-toggle-field-toggle")
        .unwrap()
        .unwrap();

    assert_eq!(
        input.get_attribute("type").as_deref(),
        Some("password"),
        "starts masked"
    );
    assert_eq!(
        input.get_attribute("name").as_deref(),
        Some("pwd"),
        "author name attr preserved"
    );
    assert_eq!(
        input.get_attribute("placeholder").as_deref(),
        Some("Password"),
        "author placeholder preserved"
    );

    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        input.get_attribute("type").as_deref(),
        Some("text"),
        "toggle reveals plain text"
    );
    assert_eq!(
        toggle.get_attribute("aria-pressed").as_deref(),
        Some("true"),
        "toggle aria-pressed mirrors visible state"
    );

    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        input.get_attribute("type").as_deref(),
        Some("password"),
        "toggle masks again"
    );

    host.remove();
}

// ─── PineOtpField ─────────────────────────────────────────────────

/// OTP Field renders N slots matching `length`, types advance
/// focus, and the accumulated value fires `pp:update:model` on
/// each keystroke. Numeric filter drops non-digit input.
#[wasm_bindgen_test]
async fn otp_field_types_digits_and_emits_value_updates() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount("<pine-otp-field length=\"4\" type=\"numeric\"></pine-otp-field>");
    tick().await;

    // Four slots rendered.
    let tag = host.query_selector("pine-otp-field").unwrap().unwrap();
    let slots = tag
        .query_selector_all("input.pine-otp-field-slot")
        .unwrap();
    assert_eq!(slots.length(), 4, "4 slots for length=4");

    // Listen for pp:update:model bubbling from the OTP field.
    let last_value: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let last = last_value.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *last.borrow_mut() = ce.detail().as_string().unwrap_or_default();
    }));
    let target: &web_sys::EventTarget = tag.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    // Helper: simulate typing `ch` into the slot at `index`.
    // `input` events from real keystrokes bubble; our test-synthesised
    // one needs bubbles=true explicitly so it reaches the delegation
    // listener on the pine-otp-field root.
    let type_into = |index: u32, ch: &str| {
        let sel = format!("input.pine-otp-field-slot[data-index=\"{index}\"]");
        let input: HtmlInputElement = tag
            .query_selector(&sel)
            .unwrap()
            .unwrap()
            .dyn_into()
            .unwrap();
        input.set_value(ch);
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        let ev = web_sys::Event::new_with_event_init_dict("input", &init).unwrap();
        input.dispatch_event(&ev).unwrap();
    };

    type_into(0, "1");
    tick().await;
    assert_eq!(&*last_value.borrow(), "1", "first digit lands");

    type_into(1, "2");
    tick().await;
    type_into(2, "3");
    tick().await;
    type_into(3, "4");
    tick().await;
    assert_eq!(&*last_value.borrow(), "1234", "full code accumulated");

    // Non-digit is filtered when type="numeric".
    type_into(3, "a");
    tick().await;
    assert_eq!(
        &*last_value.borrow(),
        "1234",
        "alphabetic typing is rejected by numeric filter"
    );

    host.remove();
}

/// After typing a character, focus auto-advances to the next
/// slot. Regression test for a bug where tick-time reconciliation
/// of the slot list stole focus back to `<body>`.
#[wasm_bindgen_test]
async fn otp_field_typing_advances_focus_to_next_slot() {
    let host = mount("<pine-otp-field length=\"4\"></pine-otp-field>");
    tick().await;

    let tag = host.query_selector("pine-otp-field").unwrap().unwrap();
    let s0: HtmlInputElement = tag
        .query_selector("input.pine-otp-field-slot[data-index=\"0\"]")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    // Focus slot 0, type a digit, wait for tick-scheduled focus move.
    s0.focus().unwrap();
    s0.set_value("1");
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    s0.dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
        .unwrap();
    // `focus_slot` defers via `setTimeout(_, 0)` so reactive
    // state flush + `pp-for` reconcile complete before the focus
    // move lands. Yield a real macrotask (not just microtasks).
    sleep_ms(5).await;
    tick().await;

    let active = doc()
        .active_element()
        .expect("document has an active element");
    assert_eq!(
        active.get_attribute("data-index").as_deref(),
        Some("1"),
        "focus advanced to slot 1 after typing in slot 0"
    );

    host.remove();
}

/// Deleting content from a filled, focused slot via the Delete
/// key (browser fires `input` with newValue "") keeps focus
/// on that same slot rather than dumping it to `<body>`.
#[wasm_bindgen_test]
async fn otp_field_delete_on_filled_slot_keeps_focus() {
    let host = mount("<pine-otp-field length=\"4\"></pine-otp-field>");
    tick().await;

    let tag = host.query_selector("pine-otp-field").unwrap().unwrap();
    let slot = |i: u32| -> HtmlInputElement {
        tag.query_selector(&format!(
            "input.pine-otp-field-slot[data-index=\"{i}\"]"
        ))
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap()
    };

    // Fill slots 0..3 with "1234" — user types each digit.
    for i in 0..4 {
        let el = slot(i);
        el.set_value(&(i + 1).to_string());
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        el.dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
            .unwrap();
        sleep_ms(5).await;
        tick().await;
    }

    // User presses Delete on slot 2 (filled). Browser clears the
    // content and fires `input` with "". Focus should stay on
    // slot 2.
    // User presses Delete on slot 2 (filled). Browser clears the
    // content and fires `input` with "". Focus should stay on
    // slot 2.
    let s2 = slot(2);
    s2.focus().unwrap();
    s2.set_value("");
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    s2.dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
        .unwrap();
    sleep_ms(5).await;
    tick().await;

    let active = doc()
        .active_element()
        .expect("document has an active element");
    assert_eq!(
        active.get_attribute("data-index").as_deref(),
        Some("2"),
        "delete on filled slot 2 keeps focus on slot 2"
    );

    host.remove();
}

/// Backspace on an empty slot moves focus *to* the previous
/// (now-cleared) slot, not to `<body>`. Regression test for
/// the same reactive-flush race that stole focus on type.
#[wasm_bindgen_test]
async fn otp_field_backspace_lands_focus_on_previous_slot() {
    let host = mount("<pine-otp-field length=\"4\"></pine-otp-field>");
    tick().await;

    let tag = host.query_selector("pine-otp-field").unwrap().unwrap();
    let slot = |i: u32| -> HtmlInputElement {
        tag.query_selector(&format!(
            "input.pine-otp-field-slot[data-index=\"{i}\"]"
        ))
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap()
    };

    // Fill slots 0..2 so the current "tail" is slot 2 (empty).
    for i in 0..2 {
        let el = slot(i);
        el.set_value(&(i + 1).to_string());
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        el.dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
            .unwrap();
    }
    // Yield so auto-advance has landed focus on slot 2.
    sleep_ms(5).await;
    tick().await;

    // User clicks / tabs to slot 2 (empty) and presses Backspace.
    let s2 = slot(2);
    s2.focus().unwrap();
    let kd_init = web_sys::KeyboardEventInit::new();
    kd_init.set_bubbles(true);
    kd_init.set_cancelable(true);
    kd_init.set_key("Backspace");
    let ev =
        web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &kd_init).unwrap();
    s2.dispatch_event(&ev).unwrap();
    sleep_ms(5).await;
    tick().await;

    let active = doc()
        .active_element()
        .expect("document has an active element");
    assert_eq!(
        active.get_attribute("data-index").as_deref(),
        Some("1"),
        "backspace on empty slot 2 focuses slot 1"
    );

    host.remove();
}

/// Backspace on an empty slot walks focus back and clears the
/// previously-filled slot. Auto-advance means a completed-then-
/// backspaced field leaves focus in the correct place.
#[wasm_bindgen_test]
async fn otp_field_backspace_walks_back_and_clears() {
    let host = mount("<pine-otp-field length=\"3\"></pine-otp-field>");
    tick().await;

    let tag = host.query_selector("pine-otp-field").unwrap().unwrap();
    let slot = |i: u32| -> HtmlInputElement {
        tag.query_selector(&format!(
            "input.pine-otp-field-slot[data-index=\"{i}\"]"
        ))
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap()
    };

    // Fill slot 0 + slot 1.
    for i in 0..2 {
        let el = slot(i);
        el.set_value(&i.to_string());
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        el.dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
            .unwrap();
    }
    tick().await;

    // Backspace-on-empty slot 2: pops slot 1's value, focuses slot 1.
    let s2 = slot(2);
    // Slot 2 is empty after auto-advance.
    assert_eq!(s2.value(), "", "slot 2 starts empty");
    let kd_init = web_sys::KeyboardEventInit::new();
    kd_init.set_bubbles(true);
    kd_init.set_cancelable(true);
    kd_init.set_key("Backspace");
    let ev =
        web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &kd_init).unwrap();
    s2.dispatch_event(&ev).unwrap();
    tick().await;

    assert_eq!(
        slot(1).value(),
        "",
        "slot 1 cleared after backspace from empty slot 2"
    );

    host.remove();
}

// ─── PineSlider ───────────────────────────────────────────────────

/// Keyboard on the thumb steps the value by Root.step; Home / End
/// snap to min / max; the emitted `pp:update:model` detail carries
/// the new numeric value.
#[wasm_bindgen_test]
async fn slider_keyboard_steps_and_emits_value() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount(
        "<pine-slider-root min=\"0\" max=\"100\" step=\"10\" value=\"50\">\
           <pine-slider-track><pine-slider-range></pine-slider-range></pine-slider-track>\
           <pine-slider-thumb></pine-slider-thumb>\
         </pine-slider-root>",
    );
    tick().await;

    let root = host.query_selector("pine-slider-root").unwrap().unwrap();
    let thumb = host.query_selector("pine-slider-thumb").unwrap().unwrap();
    let inner_thumb = host.query_selector(".pine-slider-thumb").unwrap().unwrap();

    // Listen for the emitted update events on the root tag so we
    // can inspect each step's detail payload.
    let last: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let l = last.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *l.borrow_mut() = ce.detail().as_f64();
    }));
    let target: &web_sys::EventTarget = root.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    // Helper: dispatch a keydown with the given key on the thumb's
    // inner <button>. Pine key-modifier routing reads `key`.
    let press = |key: &str| {
        let init = web_sys::KeyboardEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        init.set_key(key);
        let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
            .unwrap();
        inner_thumb.dispatch_event(&ev).unwrap();
    };

    assert_eq!(
        inner_thumb.get_attribute("aria-valuenow").as_deref(),
        Some("50"),
        "initial aria-valuenow is 50"
    );

    press("ArrowRight");
    tick().await;
    assert_eq!(*last.borrow(), Some(60.0), "ArrowRight → +step");

    press("ArrowLeft");
    tick().await;
    assert_eq!(*last.borrow(), Some(50.0), "ArrowLeft → -step");

    press("Home");
    tick().await;
    assert_eq!(*last.borrow(), Some(0.0), "Home → min");

    press("End");
    tick().await;
    assert_eq!(*last.borrow(), Some(100.0), "End → max");

    // Thumb aria-valuenow mirrors the current value.
    assert_eq!(
        inner_thumb.get_attribute("aria-valuenow").as_deref(),
        Some("100"),
        "aria-valuenow tracks the emitted value"
    );
    let _ = thumb;

    host.remove();
}

/// Pointer down on the slider (anywhere inside it — track, range,
/// or thumb) snaps the value to the click position. Regression:
/// an earlier pass installed the listener on the Track only, but
/// clicks on the Thumb don't bubble to Track (sibling in the DOM).
#[wasm_bindgen_test]
async fn slider_pointerdown_snaps_value_to_click_position() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount(
        "<pine-slider-root min=\"0\" max=\"100\" step=\"10\" value=\"0\">\
           <pine-slider-track><pine-slider-range></pine-slider-range></pine-slider-track>\
           <pine-slider-thumb></pine-slider-thumb>\
         </pine-slider-root>",
    );
    // Inject a stylesheet so the track has a measurable width —
    // without it, `getBoundingClientRect()` returns 0-wide and
    // pointer-to-value maps to the min.
    let style = doc().create_element("style").unwrap();
    style.set_text_content(Some(
        ".pine-slider-root, .pine-slider-track { display: block; width: 200px; height: 4px; position: relative; }",
    ));
    doc().head().unwrap().append_child(&style).unwrap();
    tick().await;
    sleep_ms(5).await;

    let root_tag = host.query_selector("pine-slider-root").unwrap().unwrap();
    let root_inner = host.query_selector(".pine-slider-root").unwrap().unwrap();

    // Listener for the emitted value updates.
    let last: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let l = last.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *l.borrow_mut() = ce.detail().as_f64();
    }));
    let target: &web_sys::EventTarget = root_tag.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    // Compute a click position ~70% across the slider's width.
    let rect = root_inner.get_bounding_client_rect();
    let x = rect.left() + rect.width() * 0.70;
    let y = rect.top() + rect.height() / 2.0;

    // Dispatch pointerdown at that position. The Root's installed
    // listener computes the target value and calls set_value.
    let init = web_sys::PointerEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_client_x(x as i32);
    init.set_client_y(y as i32);
    init.set_pointer_id(1);
    let ev =
        web_sys::PointerEvent::new_with_event_init_dict("pointerdown", &init).unwrap();
    root_inner.dispatch_event(&ev).unwrap();
    tick().await;

    // 70 → snaps to step=10 → 70.
    assert_eq!(
        *last.borrow(),
        Some(70.0),
        "pointerdown at ~70% snapped the value to 70"
    );

    style.remove();
    host.remove();
}

/// Pointerdown-then-pointermove (drag) updates value during
/// motion — not just on the initial press. Regression: pointer
/// capture must route `pointermove` to the root so the listener
/// fires even when the cursor moves outside the root's box.
#[wasm_bindgen_test]
async fn slider_drag_updates_value_continuously() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount(
        "<pine-slider-root min=\"0\" max=\"100\" step=\"10\" value=\"0\">\
           <pine-slider-track><pine-slider-range></pine-slider-range></pine-slider-track>\
           <pine-slider-thumb></pine-slider-thumb>\
         </pine-slider-root>",
    );
    let style = doc().create_element("style").unwrap();
    style.set_text_content(Some(
        ".pine-slider-root, .pine-slider-track { display: block; width: 200px; height: 4px; position: relative; }",
    ));
    doc().head().unwrap().append_child(&style).unwrap();
    tick().await;
    sleep_ms(5).await;

    let root_tag = host.query_selector("pine-slider-root").unwrap().unwrap();
    let root_inner = host.query_selector(".pine-slider-root").unwrap().unwrap();

    // Collect every emitted value so we can verify motion.
    let seen: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        if let Some(v) = ce.detail().as_f64() {
            s.borrow_mut().push(v);
        }
    }));
    let target: &web_sys::EventTarget = root_tag.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    let rect = root_inner.get_bounding_client_rect();
    let y = rect.top() + rect.height() / 2.0;

    let mk = |kind: &str, x: f64| -> web_sys::PointerEvent {
        let init = web_sys::PointerEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        init.set_client_x(x as i32);
        init.set_client_y(y as i32);
        init.set_pointer_id(1);
        web_sys::PointerEvent::new_with_event_init_dict(kind, &init).unwrap()
    };

    // Start drag at 20%, drag through 50% and 80%, release.
    // pointerdown lands on the slider root; pointermove / pointerup
    // go to document (where the listeners live, so drag keeps
    // working when the cursor leaves the slider's box).
    let x20 = rect.left() + rect.width() * 0.20;
    let x50 = rect.left() + rect.width() * 0.50;
    let x80 = rect.left() + rect.width() * 0.80;
    let doc_ref = doc();
    let doc_target: &web_sys::EventTarget = doc_ref.as_ref();
    root_inner.dispatch_event(&mk("pointerdown", x20)).unwrap();
    tick().await;
    doc_target.dispatch_event(&mk("pointermove", x50)).unwrap();
    tick().await;
    doc_target.dispatch_event(&mk("pointermove", x80)).unwrap();
    tick().await;
    doc_target.dispatch_event(&mk("pointerup", x80)).unwrap();
    tick().await;

    let values = seen.borrow().clone();
    assert!(
        values.contains(&20.0),
        "drag start at 20% emitted 20; got {values:?}"
    );
    assert!(
        values.contains(&50.0),
        "pointermove at 50% emitted 50; got {values:?}"
    );
    assert!(
        values.contains(&80.0),
        "pointermove at 80% emitted 80; got {values:?}"
    );

    style.remove();
    host.remove();
}

// ─── PineSelect ───────────────────────────────────────────────────

/// Clicking the Trigger opens the listbox, clicking an Item
/// selects it + closes the listbox, and the emitted
/// `pp:update:model` carries the Item's value so a parent
/// `pp-model:value` binding round-trips.
#[wasm_bindgen_test]
async fn select_click_item_emits_value_and_closes() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount(
        "<pine-select-root>\
           <pine-select-trigger>\
             <pine-select-value>Pick one</pine-select-value>\
           </pine-select-trigger>\
           <pine-select-portal>\
             <pine-select-content>\
               <pine-select-item value=\"a\">Apple</pine-select-item>\
               <pine-select-item value=\"b\">Banana</pine-select-item>\
               <pine-select-item value=\"c\" disabled>Carrot</pine-select-item>\
             </pine-select-content>\
           </pine-select-portal>\
         </pine-select-root>",
    );
    tick().await;

    let root_tag = host.query_selector("pine-select-root").unwrap().unwrap();
    let trigger: HtmlElement = host
        .query_selector("button.pine-select-trigger")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    let last: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let l = last.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *l.borrow_mut() = ce.detail().as_string();
    }));
    let target: &web_sys::EventTarget = root_tag.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    // Initial: closed, aria-expanded=false.
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false")
    );

    // Open.
    trigger.click();
    tick().await;
    let listbox = doc()
        .query_selector("ul[role=\"listbox\"].pine-select-content")
        .unwrap()
        .expect("listbox teleported to body after open");
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("true")
    );

    // Click Banana.
    let banana: HtmlElement = listbox
        .query_selector("li[data-value=\"b\"]")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    banana.click();
    tick().await;
    sleep_ms(5).await;

    assert_eq!(
        last.borrow().as_deref(),
        Some("b"),
        "pp:update:model emitted the selected value"
    );
    // Listbox closes (pp-if unmounts the portal).
    assert!(
        doc()
            .query_selector("ul[role=\"listbox\"].pine-select-content")
            .unwrap()
            .is_none(),
        "listbox closed after selection"
    );
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false")
    );

    host.remove();
}

/// Escape closes the listbox without changing the current value.
/// aria-selected on Items mirrors the Root's value so a freshly
/// opened listbox paints the current selection.
#[wasm_bindgen_test]
async fn select_escape_closes_and_aria_selected_mirrors_value() {
    let host = mount(
        "<pine-select-root value=\"b\">\
           <pine-select-trigger><pine-select-value>-</pine-select-value></pine-select-trigger>\
           <pine-select-portal>\
             <pine-select-content>\
               <pine-select-item value=\"a\">Apple</pine-select-item>\
               <pine-select-item value=\"b\">Banana</pine-select-item>\
             </pine-select-content>\
           </pine-select-portal>\
         </pine-select-root>",
    );
    tick().await;

    let trigger: HtmlElement = host
        .query_selector("button.pine-select-trigger")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    trigger.click();
    tick().await;

    let listbox = doc()
        .query_selector("ul[role=\"listbox\"].pine-select-content")
        .unwrap()
        .expect("listbox open");
    let banana = listbox
        .query_selector("li[data-value=\"b\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        banana.get_attribute("aria-selected").as_deref(),
        Some("true"),
        "initially-selected item mirrors aria-selected"
    );

    // Escape.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    listbox.dispatch_event(&ev).unwrap();
    tick().await;
    sleep_ms(5).await;

    assert!(
        doc()
            .query_selector("ul[role=\"listbox\"].pine-select-content")
            .unwrap()
            .is_none(),
        "listbox closed after Escape"
    );

    host.remove();
}

// ─── PineCombobox ─────────────────────────────────────────────────

/// Typing in the Input filters the listbox via each Item's
/// `visible` mirror + `pp-show`. Arrow keys move
/// `aria-activedescendant` on the Input without moving DOM focus.
/// Enter activates the currently-highlighted item.
#[wasm_bindgen_test]
async fn combobox_filter_arrow_nav_and_enter_selects() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount(
        "<pine-combobox-root>\
           <pine-combobox-input placeholder=\"Search\"></pine-combobox-input>\
           <pine-combobox-portal>\
             <pine-combobox-content>\
               <pine-combobox-item value=\"react\">React</pine-combobox-item>\
               <pine-combobox-item value=\"vue\">Vue</pine-combobox-item>\
               <pine-combobox-item value=\"svelte\">Svelte</pine-combobox-item>\
               <pine-combobox-empty>Nothing matched.</pine-combobox-empty>\
             </pine-combobox-content>\
           </pine-combobox-portal>\
         </pine-combobox-root>",
    );
    tick().await;

    let root_tag = host.query_selector("pine-combobox-root").unwrap().unwrap();
    let input: HtmlInputElement = host
        .query_selector("input.pine-combobox-input")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    let last: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let l = last.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *l.borrow_mut() = ce.detail().as_string();
    }));
    let target: &web_sys::EventTarget = root_tag.as_ref();
    target
        .add_event_listener_with_callback("pp:update:model", cb.as_ref().unchecked_ref())
        .unwrap();

    // Focus the input — opens the listbox.
    input.focus().unwrap();
    tick().await;
    sleep_ms(5).await;

    let listbox = doc()
        .query_selector("ul[role=\"listbox\"].pine-combobox-content")
        .expect("listbox selector query ran")
        .expect("listbox mounted after focus");

    // Type "v" — filters to "Vue" + "Svelte".
    input.set_value("v");
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    input
        .dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
        .unwrap();
    tick().await;
    sleep_ms(5).await;

    let react = listbox
        .query_selector("li[data-value=\"react\"]")
        .unwrap()
        .unwrap();
    let vue = listbox
        .query_selector("li[data-value=\"vue\"]")
        .unwrap()
        .unwrap();
    let svelte = listbox
        .query_selector("li[data-value=\"svelte\"]")
        .unwrap()
        .unwrap();

    // React should be hidden (no 'v'); Vue + Svelte visible.
    let is_hidden = |el: &Element| -> bool {
        if let Ok(html) = el.clone().dyn_into::<HtmlElement>() {
            return html
                .style()
                .get_property_value("display")
                .unwrap_or_default()
                == "none";
        }
        false
    };
    assert!(is_hidden(&react), "React filtered out by query 'v'");
    assert!(!is_hidden(&vue), "Vue still visible");
    assert!(!is_hidden(&svelte), "Svelte still visible");

    // DOM focus must still be on the input.
    assert_eq!(
        doc().active_element().unwrap(),
        input.clone().dyn_into::<Element>().unwrap(),
        "focus stays on the input while filtering"
    );

    // ArrowDown — activedescendant moves; DOM focus stays on input.
    let press = |key: &str| {
        let init = web_sys::KeyboardEventInit::new();
        init.set_key(key);
        init.set_bubbles(true);
        init.set_cancelable(true);
        let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
            .unwrap();
        input.dispatch_event(&ev).unwrap();
    };
    press("ArrowDown");
    tick().await;
    assert_eq!(
        doc().active_element().unwrap(),
        input.clone().dyn_into::<Element>().unwrap(),
        "focus stays on input after ArrowDown"
    );
    let active_after = input
        .get_attribute("aria-activedescendant")
        .expect("activedescendant set after ArrowDown");
    assert!(
        active_after == vue.id() || active_after == svelte.id(),
        "active landed on a visible item; got {active_after}"
    );

    // Enter — selects the currently-highlighted item.
    press("Enter");
    tick().await;
    sleep_ms(5).await;

    let emitted = last.borrow().clone();
    assert!(
        emitted == Some("vue".into()) || emitted == Some("svelte".into()),
        "Enter selected a visible item; emitted {emitted:?}"
    );
    assert!(
        doc()
            .query_selector("ul[role=\"listbox\"].pine-combobox-content")
            .unwrap()
            .is_none(),
        "listbox closed after Enter-select"
    );

    host.remove();
}

// ─── PineCommand ──────────────────────────────────────────────────

/// Mod+K toggles the palette open. Typing filters Items. Clicking
/// an Item fires `pp:command:select` with the item's `value`
/// and closes the palette.
#[wasm_bindgen_test]
async fn command_mod_k_opens_click_fires_select_and_closes() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount(
        "<pine-command-root>\
           <pine-command-portal>\
             <pine-command-overlay></pine-command-overlay>\
             <pine-command-content>\
               <pine-command-input placeholder=\"Type a command\"></pine-command-input>\
               <pine-command-list>\
                 <pine-command-item value=\"new\">New File</pine-command-item>\
                 <pine-command-item value=\"open\">Open File</pine-command-item>\
                 <pine-command-empty>No match.</pine-command-empty>\
               </pine-command-list>\
             </pine-command-content>\
           </pine-command-portal>\
         </pine-command-root>",
    );
    tick().await;
    sleep_ms(5).await;

    // Initially closed — no Content in body.
    assert!(
        doc()
            .query_selector("div[role=\"dialog\"].pine-command-content")
            .unwrap()
            .is_none(),
        "palette starts closed"
    );

    // Ctrl+K (Mod+K on non-macOS) opens it.
    let doc_ref = doc();
    let doc_target: &web_sys::EventTarget = doc_ref.as_ref();
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("k");
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_ctrl_key(true);
    let ev =
        web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    doc_target.dispatch_event(&ev).unwrap();
    tick().await;
    sleep_ms(5).await;

    let content = doc()
        .query_selector("div[role=\"dialog\"].pine-command-content")
        .unwrap()
        .expect("palette opened after Ctrl+K");

    // Listen for `pp:command:select` on body (event bubbles up
    // from the portal-mounted item).
    let picked: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let p = picked.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> = Closure::wrap(Box::new(move |ev: web_sys::Event| {
        let ce: web_sys::CustomEvent = ev.dyn_into().unwrap();
        *p.borrow_mut() = ce.detail().as_string();
    }));
    let body = doc().body().unwrap();
    let body_target: &web_sys::EventTarget = body.as_ref();
    body_target
        .add_event_listener_with_callback("pp:command:select", cb.as_ref().unchecked_ref())
        .unwrap();

    // Click "Open File".
    let open_item: HtmlElement = content
        .query_selector("li[data-value=\"open\"]")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    open_item.click();
    tick().await;
    sleep_ms(5).await;

    assert_eq!(
        picked.borrow().as_deref(),
        Some("open"),
        "pp:command:select fired with the item value"
    );
    assert!(
        doc()
            .query_selector("div[role=\"dialog\"].pine-command-content")
            .unwrap()
            .is_none(),
        "palette closed after item click"
    );

    host.remove();
}

/// DIAGNOSTIC: simulate keystroke-by-keystroke typing and verify
/// the input's `.value` accumulates AND `root.query` tracks it.
#[wasm_bindgen_test]
async fn combobox_accumulates_typed_characters_in_input_value() {
    let host = mount(
        "<pine-combobox-root>\
           <pine-combobox-input placeholder=\"\"></pine-combobox-input>\
           <pine-combobox-portal>\
             <pine-combobox-content>\
               <pine-combobox-item value=\"a\">Apple</pine-combobox-item>\
               <pine-combobox-item value=\"b\">Banana</pine-combobox-item>\
             </pine-combobox-content>\
           </pine-combobox-portal>\
         </pine-combobox-root>",
    );
    tick().await;

    let input: HtmlInputElement = host
        .query_selector("input.pine-combobox-input")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    input.focus().unwrap();
    tick().await;
    sleep_ms(5).await;

    // Simulate typing "re" char by char.
    let push_char = |ch: &str| {
        let current = input.value();
        input.set_value(&(current + ch));
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        input
            .dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
            .unwrap();
    };
    push_char("r");
    sleep_ms(5).await;
    tick().await;
    push_char("e");
    sleep_ms(5).await;
    tick().await;

    assert_eq!(
        input.value(),
        "re",
        "input.value accumulated 're' across two keystrokes"
    );

    host.remove();
}
