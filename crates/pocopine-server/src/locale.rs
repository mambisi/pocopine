//! Explicit request locale and catalog-backed framework rejection messages.
//! Enable the server's `locale` feature and initialize generated host catalogs
//! before constructing [`ServerLocale`]. Locale negotiation never redirects an
//! RPC request. Opt-in page routing wraps only the application page router.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{Extensions, HeaderMap, HeaderValue, header},
    middleware::Next,
    response::Response,
};
use pocopine_core::ServerError;
use pocopine_locale::{LOCALE_COOKIE, LOCALE_HEADER, LocalePreferences};
pub use pocopine_locale::{Locale, Locales};

mod routing;

/// Bind the four framework rejection classes to generated, argument-free
/// translation functions. Application errors returned by the handler keep their
/// own public message and stable classification; this adapter only handles
/// rejections before the handler runs.
#[derive(Clone, Copy)]
pub struct FrameworkMessages {
    pub unauthorized: fn(Locale) -> String,
    pub forbidden: fn(Locale) -> String,
    pub bad_request: fn(Locale) -> String,
    pub internal: fn(Locale) -> String,
}

struct PublicMessages {
    unauthorized: String,
    forbidden: String,
    bad_request: String,
    internal: String,
}

#[derive(Clone)]
struct RequestMessages(Arc<PublicMessages>);

/// A configured locale set and prepared public rejection text. Construction
/// calls the supplied generated functions for every configured locale, before
/// any request is accepted. It stores no current language.
#[derive(Clone)]
pub struct ServerLocale {
    locales: Locales,
    routes: Option<pocopine_locale::LocaleRoutes>,
    messages: BTreeMap<Locale, Arc<PublicMessages>>,
}

impl ServerLocale {
    /// Generated host catalogs must already be initialized (`t::initialize()`).
    /// A missing/broken catalog therefore fails startup, never the first user
    /// request. The generated functions use this same locale set.
    pub fn new(locales: Locales, messages: FrameworkMessages) -> Self {
        let prepared = locales
            .supported()
            .map(|locale| {
                (
                    locale.clone(),
                    Arc::new(PublicMessages {
                        unauthorized: (messages.unauthorized)(locale.clone()),
                        forbidden: (messages.forbidden)(locale.clone()),
                        bad_request: (messages.bad_request)(locale.clone()),
                        internal: (messages.internal)(locale.clone()),
                    }),
                )
            })
            .collect();
        Self {
            locales,
            routes: None,
            messages: prepared,
        }
    }
    pub fn locales(&self) -> &Locales {
        &self.locales
    }
}

/// Installed by Server::with_locale at finalization, outside auth and every
/// application/plugin layer so all pre-handler rejection paths have a locale.
pub(crate) async fn middleware(
    State(service): State<Arc<ServerLocale>>,
    mut request: Request,
    next: Next,
) -> Response {
    let accepted = combined(request.headers(), header::ACCEPT_LANGUAGE, ",");
    let cookies = combined(request.headers(), header::COOKIE, ";");
    let explicit = {
        let mut values = request.headers().get_all(LOCALE_HEADER).iter();
        let first = values.next().and_then(|value| value.to_str().ok());
        // Conflicting duplicate metadata is not an explicit user preference.
        if values.next().is_none() { first } else { None }
    };
    // Browser document requests resolve their URL before outer auth layers.
    // Explicit RPC metadata is handled by the ordinary RPC negotiation path.
    let page = service
        .routes
        .as_ref()
        .filter(|_| routing::is_document(&request))
        .and_then(|routes| {
            routes
                .resolve(
                    request
                        .uri()
                        .path_and_query()
                        .map(|v| v.as_str())
                        .unwrap_or("/"),
                    cookie(&cookies),
                    &accepted,
                    routing::visited(&cookies),
                )
                .ok()
        });
    let selection = service.locales.negotiate(LocalePreferences {
        route: page.as_ref().map(|page| page.locale.as_str()),
        explicit,
        cookie: cookie(&cookies),
        accepted: &accepted,
    });
    let locale = selection.locale.clone();
    request
        .extensions_mut()
        .insert(RequestMessages(service.messages[&locale].clone()));
    request.extensions_mut().insert(locale.clone());
    request.extensions_mut().insert(selection);
    let mut response = next.run(request).await;
    if !response.headers().contains_key(header::CONTENT_LANGUAGE) {
        response.headers_mut().insert(
            header::CONTENT_LANGUAGE,
            HeaderValue::from_str(locale.as_str()).expect("validated ASCII locale"),
        );
    }
    // Preserve existing Vary values, including '*'. Otherwise downstream
    // caches could serve a translated error/page to a different language.
    for name in [LOCALE_HEADER, "cookie", "accept-language"] {
        let present = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .any(|value| {
                value.to_str().is_ok_and(|value| {
                    value.split(',').any(|token| {
                        let token = token.trim();
                        token == "*" || token.eq_ignore_ascii_case(name)
                    })
                })
            });
        if !present {
            response
                .headers_mut()
                .append(header::VARY, HeaderValue::from_static(name));
        }
    }
    response
}

fn combined(headers: &HeaderMap, name: header::HeaderName, separator: &str) -> String {
    let mut result = String::new();
    for value in headers.get_all(name) {
        let Ok(value) = value.to_str() else {
            return String::new();
        };
        if result.len() + separator.len() + value.len() > 8192 {
            return String::new();
        }
        if !result.is_empty() {
            result.push_str(separator);
        }
        result.push_str(value);
    }
    result
}

fn cookie(cookies: &str) -> Option<&str> {
    let mut matches = cookies.split(';').filter_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == LOCALE_COOKIE).then_some(value)
    });
    let value = matches.next()?;
    matches.next().is_none().then_some(value)
}

pub(crate) fn public_rejection(error: ServerError, extensions: &Extensions) -> ServerError {
    let Some(RequestMessages(messages)) = extensions.get::<RequestMessages>() else {
        return error;
    };
    match error {
        ServerError::Unauthorized(_) => ServerError::Unauthorized(messages.unauthorized.clone()),
        ServerError::Forbidden(_) => ServerError::Forbidden(messages.forbidden.clone()),
        ServerError::BadRequest(_) => ServerError::BadRequest(messages.bad_request.clone()),
        ServerError::App(_) => ServerError::App(messages.internal.clone()),
        // Network is a client-side diagnostic and has no host translation.
        error @ ServerError::Network(_) => error,
    }
}
