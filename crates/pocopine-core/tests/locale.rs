//! Browser selection, real fetch boundaries, and reactive ownership contracts.
#![cfg(all(target_arch = "wasm32", feature = "locale"))]

use std::{
    cell::{Cell, RefCell},
    future::{Future, pending},
    rc::Rc,
    task::{Context, Poll, Waker},
};

use pocopine_core::locale::{
    CATALOG_FORMAT_VERSION, CatalogArtifact, CatalogAudience, CatalogEntry, Locale, Locales,
    MessageId, TranslationError,
    client::{LocaleController, SwitchOutcome},
};
use pocopine_core::{ServerError, effect, fetch, flush_sync, release, set_auto_flush};
use pocopine_locale::client::ClientCatalogs;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

fn locale(value: &str) -> Locale {
    value.parse().unwrap()
}
fn bytes(lang: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&CatalogArtifact {
        format_version: CATALOG_FORMAT_VERSION,
        build_id: "a".repeat(64),
        locale: locale(lang),
        audience: CatalogAudience::Browser,
        messages: vec![Some(CatalogEntry {
            source_locale: locale(lang),
            message: message.into(),
        })],
    })
    .unwrap()
}
fn cache() -> ClientCatalogs {
    ClientCatalogs::new(
        Locales::new(locale("en"), [locale("en"), locale("fr")]).unwrap(),
        &"a".repeat(64),
        1,
    )
    .unwrap()
}
fn controller() -> LocaleController {
    let cache = cache();
    cache.install(locale("en"), &bytes("en", "Ready")).unwrap();
    LocaleController::new(cache, locale("en")).unwrap()
}

#[wasm_bindgen_test]
fn selection_requires_a_ready_exact_locale_and_validation_preserves_the_old_language() {
    let cache = cache();
    assert!(matches!(
        LocaleController::new(cache.clone(), locale("en")),
        Err(TranslationError::CatalogNotLoaded(_))
    ));
    cache.install(locale("en"), &bytes("en", "Ready")).unwrap();
    assert!(LocaleController::new(cache.clone(), locale("en-US")).is_err());
    let ui = LocaleController::new(cache.clone(), locale("en")).unwrap();
    assert!(ui.begin_switch(locale("de")).is_err());
    let ticket = ui.begin_switch(locale("fr")).unwrap();
    assert!(ticket.needs_catalog());
    assert_eq!(ui.format(MessageId(0), &[]).unwrap(), "Ready");
    assert!(ticket.commit(Some(&bytes("en", "Wrong locale"))).is_err());
    let stale = String::from_utf8(bytes("fr", "Prêt"))
        .unwrap()
        .replace(&"a".repeat(64), &"b".repeat(64));
    assert!(
        ui.begin_switch(locale("fr"))
            .unwrap()
            .commit(Some(stale.as_bytes()))
            .is_err()
    );
    assert_eq!(ui.snapshot(), locale("en"));
    assert!(cache.catalog(&locale("fr")).is_err());
    ui.begin_switch(locale("fr"))
        .unwrap()
        .commit(Some(&bytes("fr", "Prêt")))
        .unwrap();
    assert_eq!(ui.snapshot(), locale("fr"));
    // The originally cloned generated-API handle observes the same install.
    assert_eq!(
        cache.format(&locale("fr"), MessageId(0), &[]).unwrap(),
        "Prêt"
    );
}

#[wasm_bindgen_test]
fn superseded_or_dropped_tickets_cannot_replace_the_latest_selection() {
    let ui = controller();
    let old = ui.begin_switch(locale("fr")).unwrap();
    // Picking the currently displayed language must cancel an older download.
    let current = ui.begin_switch(locale("en")).unwrap();
    assert!(!old.is_current());
    assert_eq!(current.commit(None).unwrap(), SwitchOutcome::Committed);
    assert_eq!(
        old.commit(Some(b"even malformed stale responses are ignored"))
            .unwrap(),
        SwitchOutcome::Superseded
    );
    let cancelled = ui.begin_switch(locale("fr")).unwrap();
    let latest = ui.begin_switch(locale("fr")).unwrap();
    drop(cancelled);
    assert!(
        latest.is_current(),
        "dropping old work must not cancel newer work"
    );
    drop(latest);
    assert_eq!(ui.snapshot(), locale("en"));
    let unloaded = ui.begin_switch(locale("fr")).unwrap();
    assert!(unloaded.commit(None).is_err());
    assert_eq!(ui.snapshot(), locale("en"));
}

#[wasm_bindgen_test]
fn asynchronous_switches_ignore_late_failures_and_cancellation() {
    let ui = controller();
    let result = Rc::new(RefCell::new(None));
    let pending_result = result.clone();
    let mut old = Box::pin(ui.switch_with(locale("fr"), move |_| {
        std::future::poll_fn(move |_| {
            pending_result
                .borrow_mut()
                .take()
                .map_or(Poll::Pending, Poll::Ready)
        })
    }));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(old.as_mut().poll(&mut cx).is_pending());
    ui.begin_switch(locale("en")).unwrap().commit(None).unwrap();
    *result.borrow_mut() = Some(Err(TranslationError::Initialization(
        "late failed fetch".into(),
    )));
    assert_eq!(
        old.as_mut().poll(&mut cx),
        Poll::Ready(Ok(SwitchOutcome::Superseded))
    );
    assert_eq!(ui.snapshot(), locale("en"));

    let mut cancelled = Box::pin(ui.switch_with(locale("fr"), |_| pending()));
    assert!(cancelled.as_mut().poll(&mut cx).is_pending());
    let latest = ui.begin_switch(locale("fr")).unwrap();
    drop(cancelled);
    assert!(latest.is_current());
    latest.commit(Some(&bytes("fr", "Prêt"))).unwrap();
    // Cache hits must not invoke the loader at all.
    let mut cached =
        Box::pin(ui.switch_with(locale("en"), |_| async { panic!("unexpected fetch") }));
    assert_eq!(
        cached.as_mut().poll(&mut cx),
        Poll::Ready(Ok(SwitchOutcome::Committed))
    );
}

#[wasm_bindgen_test]
fn translation_effects_see_ready_catalogs_and_release_without_tracking_rpc_snapshots() {
    set_auto_flush(false);
    let ui = controller();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let output = seen.clone();
    let reader = ui.clone();
    let translated = effect(move || {
        output
            .borrow_mut()
            .push(reader.format(MessageId(0), &[]).unwrap())
    });
    let snapshots = Rc::new(Cell::new(0));
    let count = snapshots.clone();
    let reader = ui.clone();
    let snapshot = effect(move || {
        reader.snapshot();
        count.set(count.get() + 1);
    });
    let fr = ui.begin_switch(locale("fr")).unwrap();
    flush_sync();
    assert_eq!(&*seen.borrow(), &["Ready"]);
    fr.commit(Some(&bytes("fr", "Prêt"))).unwrap();
    flush_sync();
    assert_eq!(&*seen.borrow(), &["Ready", "Prêt"]);
    assert_eq!(
        snapshots.get(),
        1,
        "an RPC snapshot must not cause a repeat request on locale changes"
    );
    release(translated);
    release(snapshot);
    ui.begin_switch(locale("en")).unwrap().commit(None).unwrap();
    flush_sync();
    assert_eq!(
        seen.borrow().len(),
        2,
        "released views must not keep translating"
    );
}

#[wasm_bindgen_test]
fn public_error_copy_preserves_server_messages_and_localizes_only_network_diagnostics() {
    let ui = controller();
    fn network(locale: Locale) -> String {
        if locale.as_str() == "fr" {
            "Connexion impossible."
        } else {
            "Unable to connect."
        }
        .into()
    }
    let error = ServerError::Network("private host/URL details".into());
    assert_eq!(ui.error_message(&error, network), "Unable to connect.");
    ui.begin_switch(locale("fr"))
        .unwrap()
        .commit(Some(&bytes("fr", "Prêt")))
        .unwrap();
    assert_eq!(ui.error_message(&error, network), "Connexion impossible.");
    for error in [
        ServerError::Unauthorized("Public denial".into()),
        ServerError::Forbidden("Public denial".into()),
        ServerError::BadRequest("Public denial".into()),
        ServerError::App("Public denial".into()),
    ] {
        assert_eq!(ui.error_message(&error, network), "Public denial");
    }
    assert!(
        matches!(error, ServerError::Network(details) if details == "private host/URL details")
    );
}

#[wasm_bindgen(inline_js = r#"
let originalFetch;
let requests;
let finishStream;
export function mockLocaleFetch() {
    originalFetch = window.fetch;
    requests = [];
    window.fetch = async (request) => {
        requests.push(request.headers.get('pocopine-locale'));
        if (request.headers.get('accept') === 'text/event-stream') {
            return new Response(new ReadableStream({ start(controller) {
                finishStream = () => {
                    controller.enqueue(new TextEncoder().encode('data: {"Ok":"stream payload"}\n\n'));
                    controller.close();
                };
            }}), {headers: {'content-type': 'text/event-stream'}});
        }
        return new Response('{"Ok":"buffered payload"}', {headers: {'content-type': 'application/json'}});
    };
}
export function capturedLocales() { return requests; }
export function finishLocaleStream() { finishStream(); }
export function restoreLocaleFetch() { window.fetch = originalFetch; }
"#)]
extern "C" {
    fn mockLocaleFetch();
    fn capturedLocales() -> js_sys::Array;
    fn finishLocaleStream();
    fn restoreLocaleFetch();
}

#[wasm_bindgen_test(async)]
async fn buffered_replays_and_sse_send_one_committed_locale_snapshot() {
    mockLocaleFetch();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            restoreLocaleFetch();
            fetch::__reset_middleware_chain_for_test();
        }
    }
    let _restore = Restore;
    fetch::__reset_middleware_chain_for_test();
    let ui = controller();
    ui.begin_switch(locale("fr"))
        .unwrap()
        .commit(Some(&bytes("fr", "Prêt")))
        .unwrap();
    ui.begin_switch(locale("en")).unwrap().commit(None).unwrap();
    let switched = ui.clone();
    fetch::install_middleware(
        move |request: fetch::FetchRequest, next: fetch::FetchNext| {
            let switched = switched.clone();
            async move {
                if request.is_replay_safe() {
                    let _first = next.clone().run(request.clone()).await?;
                    switched
                        .begin_switch(locale("fr"))
                        .unwrap()
                        .commit(None)
                        .unwrap();
                }
                next.run(request).await
            }
        },
    );
    // Feature-enabled applications without an active locale keep old headers.
    let _: String = fetch::call("/plain", &()).await.unwrap();
    assert!(capturedLocales().get(0).is_null());
    ui.activate().unwrap();
    ui.activate().unwrap();
    assert!(controller().activate().is_err());
    let _: String = fetch::call_replay_safe("/retry", &()).await.unwrap();
    let _: String = fetch::call("/next", &()).await.unwrap();
    let mut stream = fetch::call_stream::<_, String>("/stream", &())
        .await
        .unwrap();
    ui.begin_switch(locale("en")).unwrap().commit(None).unwrap();
    finishLocaleStream();
    assert_eq!(
        std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
            .await
            .unwrap()
            .unwrap(),
        "stream payload"
    );
    assert!(
        std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
            .await
            .is_none()
    );
    let sent = capturedLocales()
        .iter()
        .map(|value| value.as_string())
        .collect::<Vec<_>>();
    assert_eq!(
        sent,
        [
            None,
            Some("en".into()),
            Some("en".into()),
            Some("fr".into()),
            Some("fr".into())
        ]
    );
}
