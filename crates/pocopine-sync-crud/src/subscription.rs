use pocopine_sync::{CollectionSelector, Handle, SyncError, SyncResult};

use crate::{LocalResourceViewState, ResourceId};

const RESOURCE_VIEW_OBSERVE_KEY: &str = "__pp_sync_crud_resource_view";

/// Observe a typed local resource view from the current Pocopine scope.
///
/// The observer tracks the scope that owns the sync collection and is
/// released when the current consumer scope unmounts. Generated resources
/// should call this helper instead of exposing raw `CollectionState`.
///
/// Browser installs use Pocopine's scoped watcher and defer the first callback
/// to the next tick to avoid lifecycle re-entrant borrows. Native test/runtime
/// installs run synchronously and bind cleanup to the current scope explicitly.
pub fn observe_local_resource_view<C, Id, Row, F>(
    handle: Handle<C>,
    selector: CollectionSelector<C, Row>,
    callback: F,
) -> SyncResult<()>
where
    C: 'static,
    Id: ResourceId + 'static,
    Row: Clone + PartialEq + 'static,
    F: Fn(&LocalResourceViewState<Id, Row>, Option<&LocalResourceViewState<Id, Row>>) + 'static,
{
    let consumer_scope = pocopine_core::current_scope_id().ok_or_else(|| {
        SyncError::client(
            "sync CRUD resource observer used outside a component handler/lifecycle hook",
        )
    })?;
    let owner_scope = handle.scope_id();
    let source = move || {
        pocopine_core::track(owner_scope, RESOURCE_VIEW_OBSERVE_KEY);
        read_resource_view_state(&handle, selector)
    };

    #[cfg(target_arch = "wasm32")]
    {
        let _ = consumer_scope;
        pocopine_core::watch_scoped(source, callback);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let effect = pocopine_core::watch(source, callback);
        pocopine_core::on_scope_unmount_for(consumer_scope, move || {
            pocopine_core::release(effect);
        });
    }

    Ok(())
}

fn read_resource_view_state<C, Id, Row>(
    handle: &Handle<C>,
    selector: CollectionSelector<C, Row>,
) -> LocalResourceViewState<Id, Row>
where
    C: 'static,
    Id: ResourceId,
    Row: Clone,
{
    match handle.try_borrow_mut() {
        Ok(mut state) => LocalResourceViewState::from_collection_state(selector(&mut state)),
        Err(_) => {
            LocalResourceViewState::from_error("sync CRUD resource state is already borrowed")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use pocopine_core::{ComponentState, Scope};
    use pocopine_sync::{
        CollectionState, SyncCollectionName, SyncCursor, SyncPullResponse, SyncRow, SyncStreamName,
    };
    use serde::{Deserialize, Serialize};
    use wasm_bindgen::JsValue;

    use super::*;

    fn host_safe_js_value() -> JsValue {
        // `JsValue::UNDEFINED` touches imported JS statics and panics in
        // native tests. The observer tests never inspect this dummy value.
        JsValue::from_str("")
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Post {
        title: String,
    }

    #[derive(Default)]
    struct Owner {
        posts: CollectionState<Post>,
    }

    #[derive(Default)]
    struct Consumer;

    impl ComponentState for Owner {
        fn get(&self, _key: &str) -> JsValue {
            host_safe_js_value()
        }

        fn set(&mut self, _key: &str, _value: JsValue) {}

        fn invoke(&mut self, _name: &str, _args: &js_sys::Array) -> JsValue {
            host_safe_js_value()
        }

        fn keys(&self) -> &'static [&'static str] {
            &[]
        }
    }

    impl ComponentState for Consumer {
        fn get(&self, _key: &str) -> JsValue {
            host_safe_js_value()
        }

        fn set(&mut self, _key: &str, _value: JsValue) {}

        fn invoke(&mut self, _name: &str, _args: &js_sys::Array) -> JsValue {
            host_safe_js_value()
        }

        fn keys(&self) -> &'static [&'static str] {
            &[]
        }
    }

    fn posts(owner: &mut Owner) -> &mut CollectionState<Post> {
        &mut owner.posts
    }

    fn post(key: &str, title: &str, version: &str) -> SyncRow<Post> {
        SyncRow::new(
            key,
            Post {
                title: title.to_string(),
            },
        )
        .unwrap()
        .version(version)
        .unwrap()
    }

    #[test]
    fn resource_view_observer_tracks_owner_scope_updates() {
        pocopine_core::set_auto_flush(false);

        let owner_state = Rc::new(RefCell::new(Owner::default()));
        let owner_scope = Scope::new(owner_state.clone());
        let owner = Handle::new(owner_state, owner_scope.id);
        let consumer_scope = Scope::new(Rc::new(RefCell::new(Consumer)));
        let observed: Rc<RefCell<Vec<LocalResourceViewState<String, Post>>>> =
            Rc::new(RefCell::new(Vec::new()));
        let observed_for_callback = observed.clone();

        pocopine_core::scope::with_current_scope_id(consumer_scope.id, || {
            observe_local_resource_view(owner.clone(), posts, move |state, _previous| {
                observed_for_callback.borrow_mut().push(state.clone());
            })
            .unwrap();
        });

        assert_eq!(observed.borrow().len(), 1);
        assert_eq!(observed.borrow()[0].view().unwrap().rows.len(), 0);

        {
            let mut owner = owner.borrow_mut();
            let request = owner.posts.begin_initial();
            owner.posts.apply_pull(
                request,
                SyncPullResponse::snapshot(
                    SyncStreamName::new("posts").unwrap(),
                    SyncCollectionName::new("posts").unwrap(),
                    vec![post("post_1", "one", "v1")],
                    Some(SyncCursor::new("c1").unwrap()),
                ),
            );
        }
        pocopine_core::trigger_scope(owner_scope.id);
        pocopine_core::flush_sync();

        assert_eq!(observed.borrow().len(), 2);
        assert_eq!(observed.borrow()[1].view().unwrap().rows[0].id, "post_1");

        pocopine_core::trigger_scope(owner_scope.id);
        pocopine_core::flush_sync();
        assert_eq!(
            observed.borrow().len(),
            2,
            "unchanged views should not fire the callback again"
        );

        {
            let mut owner = owner.borrow_mut();
            owner.posts.set_error("network offline");
        }
        pocopine_core::trigger_scope(owner_scope.id);
        pocopine_core::flush_sync();

        assert_eq!(observed.borrow().len(), 3);
        assert_eq!(
            observed.borrow()[2].view().unwrap().error,
            "network offline"
        );

        Scope::remove(consumer_scope.id);
        {
            let mut owner = owner.borrow_mut();
            owner.posts.clear_error();
        }
        pocopine_core::trigger_scope(owner_scope.id);
        pocopine_core::flush_sync();

        assert_eq!(
            observed.borrow().len(),
            3,
            "consumer unmount should release the observer"
        );

        Scope::remove(owner_scope.id);
        pocopine_core::set_auto_flush(true);
    }

    #[test]
    fn resource_view_observer_requires_current_scope() {
        let owner_state = Rc::new(RefCell::new(Owner::default()));
        let owner_scope = Scope::new(owner_state.clone());
        let owner = Handle::new(owner_state, owner_scope.id);

        let err =
            observe_local_resource_view::<_, String, Post, _>(owner, posts, |_state, _previous| {})
                .unwrap_err();

        assert!(err.to_string().contains("outside a component"));

        Scope::remove(owner_scope.id);
    }

    #[test]
    fn resource_view_observer_reports_borrow_errors_as_state() {
        pocopine_core::set_auto_flush(false);

        let owner_state = Rc::new(RefCell::new(Owner::default()));
        let owner_scope = Scope::new(owner_state.clone());
        let owner = Handle::new(owner_state, owner_scope.id);
        let consumer_scope = Scope::new(Rc::new(RefCell::new(Consumer)));
        let observed: Rc<RefCell<Vec<LocalResourceViewState<String, Post>>>> =
            Rc::new(RefCell::new(Vec::new()));
        let observed_for_callback = observed.clone();
        let _borrow = owner.borrow_mut();

        pocopine_core::scope::with_current_scope_id(consumer_scope.id, || {
            observe_local_resource_view(owner.clone(), posts, move |state, _previous| {
                observed_for_callback.borrow_mut().push(state.clone());
            })
            .unwrap();
        });

        assert_eq!(observed.borrow().len(), 1);
        assert!(observed.borrow()[0]
            .error()
            .is_some_and(|error| error.contains("already borrowed")));

        Scope::remove(consumer_scope.id);
        Scope::remove(owner_scope.id);
        pocopine_core::set_auto_flush(true);
    }
}
