use pocopine_locale::{Locale, RoutingMode, TranslationError};
use wasm_bindgen::prelude::*;

use super::{LocaleController, SwitchOutcome, active, boot};

#[wasm_bindgen(inline_js = r#"
export function locale_remember(tag) {
  const secure = location.protocol === 'https:' ? '; Secure' : '';
  try {
    document.cookie = 'pocopine_locale_visited=1; Path=/; SameSite=Lax' + secure;
    if (tag) document.cookie = 'pocopine_locale=' + tag + '; Path=/; Max-Age=31536000; SameSite=Lax' + secure;
  } catch (_) {}
  try {
    sessionStorage.setItem('pocopine_locale_visited', '1');
    if (tag) sessionStorage.setItem('pocopine_locale', tag);
  } catch (_) {}
}
export function locale_navigation_failed(message) {
  window.dispatchEvent(new CustomEvent('pocopine:locale-error', {detail: message}));
  console.error('Locale navigation failed:', message);
}
"#)]
extern "C" {
    fn locale_remember(tag: Option<&str>);
    fn locale_navigation_failed(message: &str);
}

pub(super) fn remember(locale: Option<&Locale>) {
    locale_remember(locale.map(Locale::as_str));
}

pub(super) fn current_url() -> String {
    let location = web_sys::window().expect("browser window").location();
    format!(
        "{}{}{}",
        location.pathname().unwrap_or_else(|_| "/".into()),
        location.search().unwrap_or_default(),
        location.hash().unwrap_or_default()
    )
}

pub(super) fn history(url: &str, push: bool) -> Result<(), TranslationError> {
    if current_url() == url {
        return Ok(());
    }
    let history = web_sys::window()
        .expect("browser window")
        .history()
        .map_err(|_| error("history unavailable"))?;
    let state = history.state().unwrap_or(JsValue::NULL);
    let result = if push {
        history.push_state_with_url(&state, "", Some(url))
    } else {
        history.replace_state_with_url(&state, "", Some(url))
    };
    result.map_err(|_| error("history rejected locale URL"))
}

fn error(message: &str) -> TranslationError {
    TranslationError::Initialization(message.into())
}

/// Called after the router invalidates its previous loader token, before any
/// guards/loaders run. A pending catalog defers navigation with the old UI intact.
pub(crate) fn ready() -> bool {
    let Ok(controller) = active() else {
        return true;
    };
    let Some(routes) = controller.routes() else {
        return true;
    };
    let url = current_url();
    let route = if routes.mode() == RoutingMode::None {
        routes.resolve(&url, Some(controller.snapshot().as_str()), "", true)
    } else {
        routes.resolve(&url, None, "", true)
    };
    let route = match route {
        Ok(route) => route,
        Err(error) => {
            failed(&controller, &error);
            return false;
        }
    };
    let ticket = match controller.begin_switch(route.locale.clone()) {
        Ok(ticket) => ticket,
        Err(error) => {
            failed(&controller, &error);
            return false;
        }
    };
    let changed = route.locale != controller.snapshot();
    let canonical = route.redirect.unwrap_or_else(|| url.clone());
    if !ticket.needs_catalog() {
        let result = ticket.commit_before(None, || history(&canonical, false));
        match result {
            Ok(SwitchOutcome::Committed) => {
                *controller.0.committed_url.borrow_mut() = Some(canonical);
                if changed {
                    crate::router::locale::cache_changed();
                }
                return true;
            }
            Ok(SwitchOutcome::Superseded) => return false,
            Err(error) => {
                failed(&controller, &error);
                return false;
            }
        }
    }
    let catalog_url = controller
        .0
        .delivery
        .as_ref()
        .expect("routing manifest")
        .catalogs[&route.locale]
        .clone();
    wasm_bindgen_futures::spawn_local(async move {
        let bytes = boot::load_catalog(&catalog_url).await;
        if !ticket.is_current() || current_url() != url {
            return;
        }
        let result = bytes
            .and_then(|bytes| ticket.commit_before(Some(&bytes), || history(&canonical, false)));
        match result {
            Ok(SwitchOutcome::Committed) => {
                *controller.0.committed_url.borrow_mut() = Some(canonical);
                crate::router::locale::changed(changed);
            }
            Ok(SwitchOutcome::Superseded) => {}
            Err(error) => failed(&controller, &error),
        }
    });
    false
}

fn failed(controller: &LocaleController, error: &TranslationError) {
    if let Some(url) = controller.0.committed_url.borrow().as_ref() {
        let _ = history(url, false);
    }
    locale_navigation_failed(&error.to_string());
}

// Applications without registered routes still need picker history to restore
// translations. A registered router owns the same event once it is initialized.
pub(super) fn listen() -> Result<(), TranslationError> {
    let callback = Closure::wrap(Box::new(move |_: web_sys::Event| {
        crate::router::locale::history_navigation();
    }) as Box<dyn FnMut(web_sys::Event)>);
    web_sys::window()
        .expect("browser window")
        .add_event_listener_with_callback("popstate", callback.as_ref().unchecked_ref())
        .map_err(|_| error("cannot listen for locale history"))?;
    callback.forget();
    Ok(())
}
