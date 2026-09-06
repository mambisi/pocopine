use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pocopine_locale::{LOCALE_HEADER, LOCALE_VISITED_COOKIE, LocaleRoutes, RoutingMode};

use super::{RequestMessages, ServerLocale, combined, cookie};

impl ServerLocale {
    /// Enable page-URL interpretation before browser document auth errors.
    /// Use the same instance's `page_router` to strip prefixes/redirect pages.
    pub fn with_routing(mut self, mode: RoutingMode) -> Self {
        self.routes = Some(LocaleRoutes::new(self.locales.clone(), mode));
        self
    }

    /// Wrap only page routes, with language selection before their guards and
    /// handlers. Mount this router as the application fallback; keep RPC, asset,
    /// health and other non-page services outside it. Prefix rewriting happens
    /// before the inner router matches a route. `with_routing` is required.
    pub fn page_router(&self, pages: Router) -> Result<Router, &'static str> {
        if self.routes.is_none() {
            return Err("call ServerLocale::with_routing before page_router");
        }
        Ok(Router::new()
            .fallback_service(pages)
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(self.clone()),
                page,
            )))
    }
}

pub(super) fn visited(cookies: &str) -> bool {
    cookies
        .split(';')
        .any(|entry| entry.trim().split_once('=') == Some((LOCALE_VISITED_COOKIE, "1")))
}

pub(super) fn is_document(request: &Request) -> bool {
    matches!(*request.method(), Method::GET | Method::HEAD)
        && !request.headers().contains_key(LOCALE_HEADER)
        && (request
            .headers()
            .get("sec-fetch-dest")
            .is_some_and(|v| v == "document")
            || request.headers().get_all(header::ACCEPT).iter().any(|v| {
                v.to_str().is_ok_and(|v| {
                    v.split(',')
                        .any(|v| v.trim().split(';').next() == Some("text/html"))
                })
            }))
}

async fn page(
    State(service): State<Arc<ServerLocale>>,
    mut request: Request,
    next: Next,
) -> Response {
    let routes = service.routes.as_ref().expect("page router configuration");
    let cookies = combined(request.headers(), header::COOKIE, ";");
    let accepted = combined(request.headers(), header::ACCEPT_LANGUAGE, ",");
    let safe_method = matches!(*request.method(), Method::GET | Method::HEAD);
    let url = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or("/");
    let route = match routes.resolve(
        url,
        cookie(&cookies),
        &accepted,
        !safe_method || visited(&cookies),
    ) {
        Ok(route) => route,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let locale = route.locale;
    let mut response = if let Some(url) = route.redirect.filter(|_| safe_method) {
        (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, url)]).into_response()
    } else {
        // Preserve the URI authority/scheme and original URI extension. Only
        // the inner service's route-matching path changes.
        let mut parts = request.uri().clone().into_parts();
        let Ok(path) = route.app_url.parse() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        parts.path_and_query = Some(path);
        let Ok(uri) = Uri::from_parts(parts) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        *request.uri_mut() = uri;
        request.extensions_mut().insert(locale.clone());
        request
            .extensions_mut()
            .insert(RequestMessages(service.messages[&locale].clone()));
        request
            .extensions_mut()
            .insert(pocopine_locale::NegotiatedLocale {
                locale: locale.clone(),
                source: pocopine_locale::LocaleSource::Route,
            });
        next.run(request).await
    };
    response.headers_mut().insert(
        header::CONTENT_LANGUAGE,
        HeaderValue::from_str(locale.as_str()).expect("validated locale"),
    );
    // HTML/redirects depend on first-visit state and language preferences.
    response.headers_mut().append(
        header::VARY,
        HeaderValue::from_static("Cookie, Accept-Language"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("pocopine_locale_visited=1; Path=/; SameSite=Lax"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::{FrameworkMessages, Locale, Locales};
    use axum::{
        Extension,
        body::{Body, to_bytes},
        routing::get,
    };
    use tower::ServiceExt;

    fn locale() -> ServerLocale {
        fn text(locale: Locale) -> String {
            locale.to_string()
        }
        ServerLocale::new(
            Locales::new(
                "en".parse().unwrap(),
                ["en", "fr"].map(|s| s.parse().unwrap()),
            )
            .unwrap(),
            FrameworkMessages {
                unauthorized: text,
                forbidden: text,
                bad_request: text,
                internal: text,
            },
        )
        .with_routing(RoutingMode::PrefixExceptDefault)
    }

    #[tokio::test]
    async fn page_prefixes_match_before_handlers_and_rpc_requests_never_redirect() {
        let service = locale();
        let pages = Router::new()
            .route(
                "/",
                get(|Extension(locale): Extension<Locale>| async move { locale.to_string() }),
            )
            .route(
                "/pricing",
                get(|Extension(locale): Extension<Locale>| async move { locale.to_string() }),
            );
        let app = Router::new()
            .route(
                "/rpc",
                get(|Extension(locale): Extension<Locale>| async move { locale.to_string() }),
            )
            .fallback_service(service.page_router(pages).unwrap())
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(service),
                super::super::middleware,
            ));
        for (url, cookies, expected, redirect) in [
            ("/?x=1", "", "fr", Some("/fr/?x=1")),
            (
                "/",
                "pocopine_locale_visited=1; pocopine_locale=fr",
                "en",
                None,
            ),
            ("/pricing", "pocopine_locale=fr", "en", None),
            ("/fr/pricing", "pocopine_locale=en", "fr", None),
            ("/rpc", "", "fr", None),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(url)
                        .header(header::ACCEPT_LANGUAGE, "fr")
                        .header(header::COOKIE, cookies)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.headers()[header::CONTENT_LANGUAGE],
                expected,
                "{url}"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok()),
                redirect,
                "{url}"
            );
            if redirect.is_none() {
                assert_eq!(
                    to_bytes(response.into_body(), 1024).await.unwrap(),
                    expected,
                    "{url}"
                );
            }
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/fr/pricing")
                    .method(Method::POST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(!response.headers().contains_key(header::LOCATION));
    }

    #[tokio::test]
    async fn document_locale_precedes_outer_auth_rejections_but_rpc_metadata_wins() {
        let service = Arc::new(locale());
        let app = Router::new()
            .fallback(|| async { "unreachable" })
            .layer(axum::middleware::from_fn(
                |request: Request, _: Next| async move {
                    let error = super::super::public_rejection(
                        pocopine_core::ServerError::Unauthorized("diagnostic".into()),
                        request.extensions(),
                    );
                    (
                        StatusCode::UNAUTHORIZED,
                        error.public_message().unwrap().to_owned(),
                    )
                },
            ))
            .layer(axum::middleware::from_fn_with_state(
                service,
                super::super::middleware,
            ));
        for (url, rpc, expected) in [
            ("/fr/pricing", None, "fr"),
            ("/pricing", None, "en"),
            ("/rpc", Some("fr"), "fr"),
        ] {
            let mut request = Request::builder()
                .uri(url)
                .header(header::ACCEPT, "text/html")
                .header(header::COOKIE, "pocopine_locale=fr");
            if let Some(rpc) = rpc {
                request = request.header(LOCALE_HEADER, rpc);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                expected
            );
        }
    }
}
