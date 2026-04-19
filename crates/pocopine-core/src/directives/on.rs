//! `pp-on:event[.mod]*="handler"` — event listeners that dispatch to a
//! macro-generated handler on the scope's `ComponentState`.
//!
//! Supported modifiers:
//!
//! | modifier           | effect                                                |
//! |--------------------|-------------------------------------------------------|
//! | `prevent`          | calls `preventDefault()` before dispatch              |
//! | `stop`             | calls `stopPropagation()`                             |
//! | `self`             | only fires when `event.target === el`                  |
//! | `once`             | browser removes the listener after one fire           |
//! | `window`           | attach to `window` instead of `el`                    |
//! | `document`         | attach to `document` instead of `el`                  |
//! | `debounce[.<ms>]`  | wait `ms` (default 300) of quiet after the last event |

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Function;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{console, AddEventListenerOptions, Event, EventTarget, KeyboardEvent, Node};

use super::DirectiveCall;
use crate::expr::{self, Expr, Spanned};
use crate::magics::with_current_event;
use crate::scope::with_current_el;

pub fn run(call: &DirectiveCall) {
    let Some(event) = call.arg.clone() else { return };
    let scope_id = call.scope_id;
    let el = call.el.clone();
    let proxy = call.proxy.clone();

    let prevent = call.modifiers.iter().any(|m| m == "prevent");
    let stop = call.modifiers.iter().any(|m| m == "stop");
    let self_only = call.modifiers.iter().any(|m| m == "self");
    let once = call.modifiers.iter().any(|m| m == "once");
    let on_window = call.modifiers.iter().any(|m| m == "window");
    let on_document = call.modifiers.iter().any(|m| m == "document");
    let outside = call.modifiers.iter().any(|m| m == "outside");
    let debounce_ms: Option<u32> = parse_debounce(&call.modifiers);

    // Parse the directive value as an expression at bind time
    // (RFC-024). Call-and-assign statements + sequences land here;
    // plain handler names (`@click="on_click"`) parse to a bare
    // `Expr::Path(["on_click"])` and get rewritten to the
    // compatibility shape `on_click($event)` so existing
    // single-identifier handlers keep receiving the event as
    // their first arg.
    let ast: Rc<Spanned<Expr>> = match expr::parse(&call.value) {
        Ok(a) => Rc::new(backfill_legacy_call(a)),
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-on:{}: {} (at {}..{})",
                event, e.message, e.span.start, e.span.end
            )));
            return;
        }
    };

    // Persistent closure used by `setTimeout` in the debounce branch.
    // Built once per listener so rapid events don't allocate a fresh
    // JS closure each time. Pulls the most recent event out of the
    // shared slot so the expression still sees `$event` after a
    // debounce delay.
    let last_event: Rc<RefCell<Option<Event>>> = Rc::new(RefCell::new(None));
    let invoke_fn: Function = {
        let ast = ast.clone();
        let last_event = last_event.clone();
        let el_for_debounce = el.clone();
        let proxy_for_debounce = proxy.clone();
        let c = Closure::wrap(Box::new(move || {
            let ev = last_event.borrow().clone();
            let ev_js: JsValue = match &ev {
                Some(e) => {
                    let r: &JsValue = e.as_ref();
                    r.clone()
                }
                None => JsValue::UNDEFINED,
            };
            with_current_el(&el_for_debounce, || {
                crate::scope::with_current_scope_id(scope_id, || {
                    with_current_event(&ev_js, || {
                        expr::evaluate(&ast, &proxy_for_debounce);
                    });
                });
            });
        }) as Box<dyn FnMut()>);
        let f: Function = c.as_ref().unchecked_ref::<Function>().clone();
        c.forget();
        f
    };

    let window = web_sys::window().expect("window");
    let timer: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));

    // Key modifiers (RFC-013). Cloned into the closure below.
    let key_modifiers: Vec<String> = call
        .modifiers
        .iter()
        .filter(|m| is_key_modifier(m))
        .cloned()
        .collect();

    let el_for_closure = el.clone();
    let closure = Closure::wrap(Box::new({
        let ast = ast.clone();
        let proxy = proxy.clone();
        let invoke_fn = invoke_fn.clone();
        let window = window.clone();
        let timer = timer.clone();
        let last_event = last_event.clone();
        move |ev: Event| {
            // `outside` runs before `prevent` so an outside click on
            // a real link still navigates (preventing default on a
            // document-level click listener would break the page).
            if outside {
                let host_node: &Node = el_for_closure.as_ref();
                if !host_node.is_connected() {
                    return;
                }
                match ev.target() {
                    Some(t) => match t.dyn_into::<Node>() {
                        Ok(node) => {
                            if el_for_closure.contains(Some(&node)) {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    None => return,
                }
            } else if prevent {
                ev.prevent_default();
            }
            if stop {
                ev.stop_propagation();
            }
            if !key_modifiers.is_empty() && !key_filter_matches(&ev, &key_modifiers) {
                return;
            }
            if self_only {
                if outside {
                    // `.self` + `.outside` is contradictory by
                    // definition — never fire.
                    return;
                }
                if let Some(target) = ev.target() {
                    if target != *el_for_closure.as_ref() {
                        return;
                    }
                }
            }
            if let Some(ms) = debounce_ms {
                // Remember the most recent event so the delayed
                // callback can still pass it to the handler.
                *last_event.borrow_mut() = Some(ev);
                if let Some(prev) = timer.take() {
                    window.clear_timeout_with_handle(prev);
                }
                let handle = window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        &invoke_fn,
                        ms as i32,
                    )
                    .unwrap_or(0);
                timer.set(Some(handle));
            } else {
                let ev_js: JsValue = {
                    let r: &JsValue = ev.as_ref();
                    r.clone()
                };
                with_current_el(&el_for_closure, || {
                    crate::scope::with_current_scope_id(scope_id, || {
                        with_current_event(&ev_js, || {
                            expr::evaluate(&ast, &proxy);
                        });
                    });
                });
            }
        }
    }) as Box<dyn FnMut(Event)>);

    let target: EventTarget = if outside || on_document {
        web_sys::window()
            .and_then(|w| w.document())
            .expect("document")
            .into()
    } else if on_window {
        web_sys::window().expect("window").into()
    } else {
        el.clone().into()
    };

    let opts = AddEventListenerOptions::new();
    opts.set_once(once);
    if outside {
        // Capture so this runs before descendants can stop the
        // event — otherwise a child's `@click.stop` hides the
        // outside click from us.
        opts.set_capture(true);
    }
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        &event,
        closure.as_ref().unchecked_ref(),
        &opts,
    );
    closure.forget();
}

/// Backward-compat: a directive value that's a single identifier
/// (`@click="on_click"`, `@submit="save"`) is rewritten to a
/// one-arg call of `on_click($event)`. Keeps every existing handler
/// that declared `(&mut self, ev: Event)` working unchanged without
/// the author having to write `@click="on_click($event)"`.
fn backfill_legacy_call(ast: Spanned<Expr>) -> Spanned<Expr> {
    match ast.value {
        Expr::Path(ref segs) if segs.len() == 1 => {
            let name = segs[0].clone();
            let span = ast.span.clone();
            let event_arg = Spanned {
                value: Expr::Path(vec!["$event".to_string()]),
                span: span.clone(),
            };
            Spanned {
                value: Expr::Call(name, vec![event_arg]),
                span,
            }
        }
        _ => ast,
    }
}

/// Scan the modifier list for `debounce` and the optional numeric
/// modifier that follows it (`pp-on:input.debounce.500="foo"` →
/// 500ms; bare `debounce` → 300ms default).
fn parse_debounce(modifiers: &[String]) -> Option<u32> {
    for (i, m) in modifiers.iter().enumerate() {
        if m == "debounce" {
            let ms = modifiers
                .get(i + 1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(300);
            return Some(ms);
        }
    }
    None
}

/// Modifiers that participate in the RFC-013 key filter. Everything
/// else (`prevent`, `stop`, `self`, `once`, `window`, `document`,
/// `debounce`, `outside`, numeric debounce args) is handled elsewhere
/// and must be explicitly excluded so a directive like
/// `pp-on:click.outside` doesn't accidentally treat `outside` as a
/// keyboard key name and filter out every non-keyboard event.
fn is_key_modifier(m: &str) -> bool {
    if matches!(
        m,
        "prevent" | "stop" | "self" | "once" | "window" | "document" | "debounce" | "outside"
    ) {
        return false;
    }
    matches!(m, "ctrl" | "shift" | "alt" | "meta")
        || named_key_for(m).is_some()
        || m.len() == 1  // single-letter key shortcut (e.g. `.k`, `.a`)
        || is_word_key(m)
}

/// Is this a named keyboard event modifier? Returns the
/// lowercase `KeyboardEvent.key` we expect to see.
fn named_key_for(m: &str) -> Option<&'static str> {
    Some(match m {
        "escape" | "esc" => "escape",
        "enter" => "enter",
        "tab" => "tab",
        "space" => " ",
        "backspace" => "backspace",
        "delete" | "del" => "delete",
        "arrow-up" | "up" => "arrowup",
        "arrow-down" | "down" => "arrowdown",
        "arrow-left" | "left" => "arrowleft",
        "arrow-right" | "right" => "arrowright",
        "home" => "home",
        "end" => "end",
        "page-up" => "pageup",
        "page-down" => "pagedown",
        _ => return None,
    })
}

/// Word-shaped key names that literally match the lowercased key
/// (so `.slash` could match `"/"` — but no built-in symbol aliases
/// today; extend here if needed).
fn is_word_key(m: &str) -> bool {
    !m.is_empty() && m.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Return true iff the event passes every key modifier. Non-keyboard
/// events fail any key modifier check.
fn key_filter_matches(ev: &Event, modifiers: &[String]) -> bool {
    let Ok(ke) = ev.clone().dyn_into::<KeyboardEvent>() else {
        return false;
    };
    let key = ke.key().to_lowercase();
    for m in modifiers {
        match m.as_str() {
            "ctrl" if !ke.ctrl_key() => return false,
            "shift" if !ke.shift_key() => return false,
            "alt" if !ke.alt_key() => return false,
            "meta" if !ke.meta_key() => return false,
            "ctrl" | "shift" | "alt" | "meta" => continue,
            _ => {
                let want = named_key_for(m).unwrap_or(m);
                if key != want {
                    return false;
                }
            }
        }
    }
    true
}
