//! Client entry: wire routes, mount the app.

#[cfg(feature = "shell")]
use pocopine::prelude::*;
#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
use pocopine::{App, Component};
#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
use std::cell::RefCell;
#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
use wasm_bindgen::prelude::*;
#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
use web_sys::Element;

#[cfg(feature = "shell")]
use crate::components::AppShell;
#[cfg(feature = "route-not-found")]
use crate::components::NotFound;
#[cfg(feature = "route-detail")]
use crate::components::StoryDetail;
#[cfg(feature = "route-list")]
use crate::components::StoryList;

#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
thread_local! {
    static ROUTE_HANDLE: RefCell<Option<pocopine::SubtreeHandle>> = const { RefCell::new(None) };
}

#[wasm_bindgen(start)]
#[cfg(feature = "shell")]
pub fn main() {
    App::new().register::<AppShell>().with_devtools().run();
}

#[wasm_bindgen]
#[cfg(feature = "route-list")]
pub fn mount_hn_route(outlet: Element, _path: String) {
    mount_component::<StoryList>(&outlet, |_| {});
}

#[wasm_bindgen]
#[cfg(feature = "route-detail")]
pub fn mount_hn_route(outlet: Element, path: String) {
    mount_component::<StoryDetail>(&outlet, |host| {
        if let Some(id) = path.strip_prefix("/item/").filter(|id| !id.is_empty()) {
            let _ = host.set_attribute("id", id);
        }
    });
}

#[wasm_bindgen]
#[cfg(feature = "route-not-found")]
pub fn mount_hn_route(outlet: Element, _path: String) {
    mount_component::<NotFound>(&outlet, |_| {});
}

#[wasm_bindgen]
#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
pub fn unmount_hn_route() {
    ROUTE_HANDLE.with(|handle| {
        if let Some(handle) = handle.borrow_mut().take() {
            handle.unmount();
        }
    });
}

#[cfg(any(
    feature = "route-list",
    feature = "route-detail",
    feature = "route-not-found"
))]
fn mount_component<C: Component>(outlet: &Element, configure: impl FnOnce(&Element)) {
    unmount_hn_route();

    let Some(document) = web_sys::window().and_then(|win| win.document()) else {
        return;
    };
    let Ok(host) = document.create_element(C::NAME) else {
        return;
    };
    configure(&host);
    outlet.replace_children_with_node_1(host.as_ref());

    let handle = App::mount_subtree::<C>(&host);
    ROUTE_HANDLE.with(|slot| {
        *slot.borrow_mut() = Some(handle);
    });
}
