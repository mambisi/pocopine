//! pocopine devtools — a floating overlay that lists live scopes,
//! their current state, and registered refs. Opt-in via
//! [`crate::app::App::with_devtools`].
//!
//! Activation:
//! - Opt-in at startup: `App::new().with_devtools().run()`.
//! - Toggle visibility with `Ctrl+Shift+D`.
//!
//! Rendering model: a `setInterval(200ms)` snapshot, not a reactive
//! subscription. A dev panel doesn't need sub-frame latency, and the
//! snapshot approach keeps the reactive core untouched.
//!
//! Architecture: the shell (root element, header, meta line, body
//! host) lives in `shell.rs`; panels implement the [`panel::Panel`]
//! trait and register themselves at install time; `event.rs` holds
//! the single delegated click router. PR A ships one panel (scope
//! inspector) — the tab strip is latent until a second panel
//! registers.

use std::cell::{Cell, RefCell};

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{window, HtmlElement, KeyboardEvent};

use crate::scope::Scope;

mod event;
mod health;
mod highlight;
pub mod hooks;
mod inspect;
mod panel;
mod panels;
pub mod ring;
mod router_log;
mod shell;
mod signal_last;
mod style;
mod util;

type RenderClosure = Closure<dyn FnMut()>;
type KeyClosure = Closure<dyn FnMut(KeyboardEvent)>;

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static INTERVAL_ID: Cell<Option<i32>> = const { Cell::new(None) };
    static RENDER_CB: RefCell<Option<RenderClosure>> = RefCell::new(None);
    static KEY_CB: RefCell<Option<KeyClosure>> = RefCell::new(None);
}

/// Install the devtools overlay. Idempotent — calling twice is a
/// no-op. Wires up the `Ctrl+Shift+D` toggle and starts the render
/// loop. Safe to call before `walker::start` — we lazy-attach the
/// panel to `<body>` on the first render.
pub fn install() {
    if INSTALLED.with(|c| c.get()) {
        return;
    }
    INSTALLED.with(|c| c.set(true));

    style::inject();
    attach_key_listener();
    register_builtin_panels();
    register_default_hooks();
    start_render_loop();
}

fn register_builtin_panels() {
    panel::register(Box::new(panels::scope::ScopeInspector));
    panel::register(Box::new(panels::timeline::Timeline));
    panel::register(Box::new(panels::graph::Graph));
    panel::register(Box::new(panels::health::Health));
    panel::register(Box::new(panels::router::RouterPanel));
}

/// Register the default push-style handlers: effect-run / handler-
/// invoke events funnel into the timeline ring; signal triggers
/// stamp a last-changed timestamp; queue changes and route changes
/// are stored in their own side tables for PR D/F panels.
fn register_default_hooks() {
    use std::rc::Rc;

    // Timeline: effect runs.
    hooks::set_on_effect_run(Rc::new(|id, scope, dur| {
        ring::push_effect_run(id, scope, dur);
    }));

    // Timeline: handler invocations. Build a short args summary
    // inline — `Array::length` is cheap, stringifying each value
    // through the shared `util::stringify` caps individual slots at
    // ~80 chars; joined total clamped at 200 so the timeline row
    // stays one line.
    hooks::set_on_handler_invoke(Rc::new(|scope, name, args, dur| {
        let summary = summarise_args(args);
        ring::push_handler(scope, name, summary, dur);
    }));

    // Signal triggers → write last-changed.
    hooks::set_on_signal_trigger(Rc::new(|id| {
        signal_last::record(id);
    }));

    // Queue changes — PR D reads `reactive::queue_snapshot` each
    // tick, so the hook is a no-op placeholder today. If we ever
    // need a queue-transitions log, push into a ring here.
    hooks::set_on_queue_change(Rc::new(|_added| {}));

    // Route changes → `router_log` ring for the PR F router panel.
    hooks::set_on_route_change(Rc::new(|path, params| {
        router_log::push(path, params);
    }));
}

fn summarise_args(args: &js_sys::Array) -> String {
    const MAX_TOTAL: usize = 200;
    let n = args.length();
    if n == 0 {
        return String::new();
    }
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&util::stringify(&args.get(i)));
        if out.chars().count() >= MAX_TOTAL {
            break;
        }
    }
    if out.chars().count() > MAX_TOTAL {
        out.truncate(
            out.char_indices()
                .nth(MAX_TOTAL)
                .map(|(i, _)| i)
                .unwrap_or(out.len()),
        );
        out.push('…');
    }
    out
}

fn attach_key_listener() {
    let Some(win) = window() else { return };
    let cb: KeyClosure = Closure::wrap(Box::new(move |ev: KeyboardEvent| {
        if ev.ctrl_key() && ev.shift_key() && ev.key().eq_ignore_ascii_case("d") {
            ev.prevent_default();
            toggle();
        }
    }));
    let _ = win.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
    KEY_CB.with(|c| *c.borrow_mut() = Some(cb));
}

fn start_render_loop() {
    let Some(win) = window() else { return };
    let cb: RenderClosure = Closure::wrap(Box::new(move || {
        render();
    }));
    if let Ok(id) = win.set_interval_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        200,
    ) {
        INTERVAL_ID.with(|c| c.set(Some(id)));
    }
    RENDER_CB.with(|c| *c.borrow_mut() = Some(cb));
    // Fire one immediate render so the panel appears right away.
    render();
}

/// Toggle overlay visibility. Bound to `Ctrl+Shift+D` at install time.
pub fn toggle() {
    let next = !shell::is_visible();
    shell::set_visible(next);
    if let Some(root) = shell::panel_root() {
        if let Ok(html_el) = root.clone().dyn_into::<HtmlElement>() {
            let style = html_el.style();
            let _ = style.set_property("display", if next { "block" } else { "none" });
        }
    }
}

/// Render pass — called by the 200ms interval and by interactive
/// code paths after state changes (panel action handlers). Hits
/// the shell chrome first, then delegates to the active panel via
/// the panel registry.
fn render() {
    if !INSTALLED.with(|c| c.get()) {
        return;
    }
    let Some(doc) = window().and_then(|w| w.document()) else { return };
    let root = shell::ensure_root(&doc);

    // Visibility — flip `display` on the root without touching the
    // rest of the shell when the panel is hidden.
    let Ok(html_el) = root.clone().dyn_into::<HtmlElement>() else { return };
    if !shell::is_visible() {
        let _ = html_el.style().set_property("display", "none");
        return;
    } else {
        let _ = html_el.style().remove_property("display");
    }

    shell::build_shell_once(&root);
    shell::update_collapse_state(&root);
    shell::update_inspect_button(&root);
    shell::update_tab_strip(&root);

    if shell::is_collapsed() {
        return;
    }

    // Sample health counters every tick so the Health panel's
    // sparklines fill whether or not it's the active tab.
    health::sample_tick();

    let scopes = Scope::all();
    shell::update_meta_line(&root, &scopes);

    // Ensure a host <div> exists for every registered panel, then
    // ask the panel registry to render whichever one is active.
    panel::ensure_mounted(|idx, id| shell::host_for_panel(&doc, idx, id));
    // `ensure_mounted` short-circuits after first mount; re-apply
    // display: none/"" every tick so `select-panel` actions take
    // visible effect.
    shell::sync_panel_host_visibility(&doc);

    let active_idx = panel::active_index();
    if let Some(active_id) = panel::summary()
        .into_iter()
        .nth(active_idx)
        .map(|(id, _, _)| id)
    {
        let host_id = format!("__pp_dev_panel_{active_id}");
        if let Some(host) = doc.get_element_by_id(&host_id) {
            panel::render_active(&host);
        }
    }
}
