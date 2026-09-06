use futures::StreamExt;
use pocopine::{ServerError, ServerResult, StreamServerResult};
use pocopine_locale::{LOCALE_HEADER, Locale, Locales};
use pocopine_server::axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, header},
};
use pocopine_server::locale::{FrameworkMessages, ServerLocale};
use pocopine_server::tower::ServiceExt;
use pocopine_server::{Extension, RequestContext, Server};

use crate::t;

#[pocopine::server(public)]
async fn localized(locale: Extension<Locale>, count: u32) -> ServerResult<String> {
    if count == 0 {
        // Handler-owned public messages remain verbatim at the boundary.
        Err(ServerError::unauthorized(t::auth::denied(locale.0)))
    } else {
        Ok(t::cart::items(locale.0, u64::from(count).into()))
    }
}

#[pocopine::server(guard = pocopine_server::auth::require_login)]
async fn protected() -> ServerResult<String> {
    Ok("unreachable".into())
}

async fn deny(ctx: RequestContext) -> ServerResult<()> {
    assert!(ctx.extension::<Locale>().is_some());
    Err(ServerError::forbidden("PRIVATE_GUARD_DETAIL"))
}
#[pocopine::server(guard = deny)]
async fn forbidden() -> ServerResult<String> {
    Ok("unreachable".into())
}

#[derive(Clone)]
struct Missing;
#[pocopine::server(public)]
async fn missing_extension(_value: Extension<Missing>) -> ServerResult<String> {
    Ok("unreachable".into())
}

#[pocopine::server(public)]
async fn stream_locale(locale: Extension<Locale>, count: u32) -> StreamServerResult<String> {
    Ok(
        futures::stream::unfold((locale.0, count), |(locale, remaining)| async move {
            if remaining == 0 {
                return None;
            }
            tokio::task::yield_now().await;
            Some((
                Ok(t::cart::items(locale.clone(), 2u64.into())),
                (locale, remaining - 1),
            ))
        })
        .boxed(),
    )
}

struct AuthSeesLocale;
impl pocopine_server::auth::AuthProvider for AuthSeesLocale {
    fn authenticate<'a>(
        &'a self,
        ctx: &'a RequestContext,
    ) -> pocopine_server::auth::AuthFuture<'a, Option<pocopine_server::auth::AuthUser>> {
        assert!(
            ctx.extension::<Locale>().is_some(),
            "locale must precede auth"
        );
        Box::pin(async { Ok(None) })
    }
}

fn service() -> ServerLocale {
    t::initialize().unwrap();
    ServerLocale::new(
        Locales::new(
            "en".parse().unwrap(),
            ["en", "fr"].map(|s| s.parse().unwrap()),
        )
        .unwrap(),
        FrameworkMessages {
            unauthorized: t::common::unauthorized,
            forbidden: t::common::forbidden,
            bad_request: t::common::bad_request,
            internal: t::common::internal,
        },
    )
}

async fn request(
    router: &Router,
    path: &str,
    body: String,
    headers: &[(&str, &str)],
) -> pocopine_server::axum::response::Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

async fn json(response: pocopine_server::axum::response::Response) -> ServerResult<String> {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn stream(response: pocopine_server::axum::response::Response) -> Vec<ServerResult<String>> {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let mut decoder = pocopine::sse::SseDecoder::new();
    let mut payloads = decoder.push(&bytes);
    if let Some(value) = decoder.flush() {
        payloads.push(value);
    }
    let mut items = Vec::new();
    let mut done = false;
    for payload in payloads {
        match pocopine::sse::decode_payload::<String>(&payload) {
            pocopine::sse::Decoded::Item(item) => items.push(item),
            pocopine::sse::Decoded::Done => done = true,
        }
    }
    assert!(done);
    items
}

#[tokio::test]
async fn locale_precedes_auth_and_body_rejections_and_remains_fixed_for_streams() {
    let router = Server::new(Router::new())
        .with_auth(AuthSeesLocale)
        .with_locale(service())
        // A route added after configuration also sees the boundary.
        .route(
            "/late",
            pocopine_server::axum::routing::post(
                |pocopine_server::axum::Extension(locale): pocopine_server::axum::Extension<
                    Locale,
                >| async move { locale.to_string() },
            ),
        )
        .route(
            "/cache-star",
            pocopine_server::axum::routing::post(|| async {
                ([("vary", "*"), ("content-language", "de")], "Deutsch")
            }),
        )
        .try_finalize()
        .unwrap();

    for (headers, expected, language) in [
        (
            vec![
                (LOCALE_HEADER, "fr"),
                ("cookie", "pocopine_locale=en"),
                ("accept-language", "en"),
            ],
            "2 articles",
            "fr",
        ),
        (
            vec![(LOCALE_HEADER, "ja"), ("cookie", "pocopine_locale=fr")],
            "2 articles",
            "fr",
        ),
        (
            vec![("accept-language", "en;q=0.5,fr-CA;q=1")],
            "2 articles",
            "fr",
        ),
        (
            vec![
                (LOCALE_HEADER, "fr"),
                (LOCALE_HEADER, "en"),
                ("accept-language", "en"),
            ],
            "2 items",
            "en",
        ),
        (
            vec![
                ("cookie", "pocopine_locale=en; pocopine_locale=fr"),
                ("accept-language", "fr"),
            ],
            "2 articles",
            "fr",
        ),
    ] {
        let response = request(&router, __localized_path(), "[2]".into(), &headers).await;
        assert_eq!(response.headers()[header::CONTENT_LANGUAGE], language);
        assert!(
            !response.headers().contains_key(header::LOCATION),
            "RPC must not redirect"
        );
        let vary = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();
        for name in [LOCALE_HEADER, "cookie", "accept-language"] {
            assert!(vary.contains(&name));
        }
        assert_eq!(json(response).await.unwrap(), expected);
    }
    let late = request(&router, "/late", "null".into(), &[(LOCALE_HEADER, "fr")]).await;
    assert_eq!(
        to_bytes(late.into_body(), 64).await.unwrap().as_ref(),
        b"fr"
    );
    let cached = request(
        &router,
        "/cache-star",
        "null".into(),
        &[(LOCALE_HEADER, "fr")],
    )
    .await;
    assert_eq!(cached.headers()[header::CONTENT_LANGUAGE], "de");
    let vary: Vec<_> = cached.headers().get_all(header::VARY).iter().collect();
    assert_eq!(vary.len(), 1);
    assert_eq!(vary[0], "*");

    let headers = [(LOCALE_HEADER, "fr")];
    for body in [
        "invalid json".to_string(),
        "x".repeat(pocopine_server::server_function_body_limit() + 1),
    ] {
        let error = json(request(&router, __localized_path(), body, &headers).await)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ServerError::BadRequest(message) if message == "Requête invalide.")
        );
        assert_eq!(error.public_message(), Some("Requête invalide."));
    }
    let error = json(request(&router, __protected_path(), "invalid json".into(), &headers).await)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ServerError::Unauthorized(message) if message == "Veuillez vous connecter.")
    );
    let error = json(request(&router, __forbidden_path(), "null".into(), &headers).await)
        .await
        .unwrap_err();
    assert!(matches!(&error, ServerError::Forbidden(message) if message == "Accès interdit."));
    let error = json(request(&router, __missing_extension_path(), "null".into(), &headers).await)
        .await
        .unwrap_err();
    assert!(matches!(&error, ServerError::App(message) if message == "Une erreur est survenue."));
    let error = json(request(&router, __localized_path(), "[0]".into(), &headers).await)
        .await
        .unwrap_err();
    assert!(matches!(&error, ServerError::Unauthorized(message) if message == "Refusé"));
    assert_eq!(
        ServerError::Network("private transport details".into()).public_message(),
        None
    );

    let (fr, en) = futures::join!(
        request(&router, __stream_locale_path(), "[3]".into(), &headers),
        request(
            &router,
            __stream_locale_path(),
            "[3]".into(),
            &[(LOCALE_HEADER, "en")]
        ),
    );
    let (fr, en) = futures::join!(stream(fr), stream(en));
    assert_eq!(
        fr.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec!["2 articles"; 3]
    );
    assert_eq!(
        en.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec!["2 items"; 3]
    );
    let errors = stream(
        request(
            &router,
            __stream_locale_path(),
            "invalid json".into(),
            &headers,
        )
        .await,
    )
    .await;
    assert_eq!(errors.len(), 1);
    assert!(
        matches!(&errors[0], Err(ServerError::BadRequest(message)) if message == "Requête invalide.")
    );

    // The same new macro/runtime preserve existing behavior without locale
    // configuration; no inferred global selection leaks from the other router.
    let plain = __localized_route(Router::new());
    let error = json(request(&plain, __localized_path(), "invalid json".into(), &headers).await)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ServerError::BadRequest(message) if message.starts_with("parse server-function request body:"))
    );
}
