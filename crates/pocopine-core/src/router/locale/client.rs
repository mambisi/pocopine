use crate::locale::{
    RoutingMode,
    client::{active, navigation},
};
use std::borrow::Cow;

pub(crate) fn ready() -> bool {
    navigation::ready()
}

pub(crate) fn app_path(path: &str) -> Cow<'_, str> {
    active()
        .ok()
        .and_then(|c| c.routes())
        .and_then(|r| r.app_url(path).ok())
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(path))
}

pub(crate) fn href(path: String) -> String {
    active()
        .ok()
        .and_then(|c| c.href(&path).ok())
        .unwrap_or(path)
}

pub(crate) fn can_prefetch(url: &str) -> bool {
    let Ok(controller) = active() else {
        return true;
    };
    let Some(routes) = controller.routes() else {
        return true;
    };
    routes.mode() == RoutingMode::None
        || routes
            .resolve(url, None, "", true)
            .is_ok_and(|route| route.locale == controller.snapshot())
}

pub(crate) fn cache_changed() {
    // Loader results fetched in one locale cannot satisfy a later language.
    super::super::clear_prefetch_state();
}

pub(crate) fn changed(changed: bool) {
    if changed {
        cache_changed();
    }
    if super::super::INITIALISED.with(|value| value.get()) {
        let _ = super::super::mount_current_or_defer();
    }
}

pub(crate) fn history_navigation() {
    if !super::super::INITIALISED.with(|value| value.get()) {
        let _ = ready();
    }
}
