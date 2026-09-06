//! Shared page-URL policy. RPC endpoints never pass through these helpers.

use crate::{Locale, LocalePreferences, Locales, RoutingMode, TranslationError};

/// Session marker, separate from the persistent explicit picker preference.
pub const LOCALE_VISITED_COOKIE: &str = "pocopine_locale_visited";

#[derive(Clone, Debug)]
pub struct LocaleRoutes {
    locales: Locales,
    mode: RoutingMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleRoute {
    pub locale: Locale,
    /// URL with a recognized locale segment removed; query/hash are preserved.
    pub app_url: String,
    /// Canonical page URL, if a redirect/replacement is necessary.
    pub redirect: Option<String>,
}

impl LocaleRoutes {
    pub fn new(locales: Locales, mode: RoutingMode) -> Self {
        Self { locales, mode }
    }

    pub fn mode(&self) -> RoutingMode {
        self.mode
    }

    /// Only exact configured tags are route segments; matching is ASCII-case
    /// insensitive, and output uses the configured canonical spelling.
    pub fn split<'a>(&self, url: &'a str) -> (Option<&Locale>, &'a str) {
        if self.mode == RoutingMode::None {
            return (None, url);
        }
        let Some(tail) = url.strip_prefix('/') else {
            return (None, url);
        };
        let end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
        let locale = self
            .locales
            .supported()
            .find(|locale| locale.as_str().eq_ignore_ascii_case(&tail[..end]));
        match locale {
            Some(locale) => (Some(locale), &tail[end..]),
            None => (None, url),
        }
    }

    pub fn app_url(&self, url: &str) -> Result<String, TranslationError> {
        safe_url(url)?;
        let (_, tail) = self.split(url);
        let path = if tail.starts_with('/') {
            tail.to_owned()
        } else {
            format!("/{tail}")
        };
        safe_url(&path)?;
        Ok(path)
    }

    /// Generate a same-origin link, replacing any existing locale segment.
    /// Query and fragment bytes are copied, never decoded/re-encoded.
    pub fn href(&self, locale: &Locale, url: &str) -> Result<String, TranslationError> {
        if !self
            .locales
            .supported()
            .any(|configured| configured == locale)
        {
            return Err(invalid("link locale is not configured"));
        }
        let app = self.app_url(url)?;
        if self.mode == RoutingMode::None
            || (self.mode == RoutingMode::PrefixExceptDefault
                && locale == self.locales.default_locale())
        {
            return Ok(app);
        }
        Ok(format!("/{locale}{app}"))
    }

    /// Resolve a page before its loader. Detection only seeds the first visit
    /// to the bare site root. Every other unprefixed page means the default
    /// language, so an explicit URL remains usable despite stored preferences.
    /// In `none` mode, cookie/browser preferences choose the language directly.
    pub fn resolve(
        &self,
        url: &str,
        cookie: Option<&str>,
        accepted: &str,
        visited: bool,
    ) -> Result<LocaleRoute, TranslationError> {
        let app_url = self.app_url(url)?;
        let (route, _) = self.split(url);
        let bare_root = url.split(['?', '#']).next() == Some("/");
        let locale = if let Some(route) = route {
            route.clone()
        } else if self.mode == RoutingMode::None || (!visited && bare_root) {
            self.locales
                .negotiate(LocalePreferences {
                    cookie,
                    accepted,
                    ..Default::default()
                })
                .locale
        } else {
            self.locales.default_locale().clone()
        };
        let canonical = self.href(&locale, url)?;
        Ok(LocaleRoute {
            locale,
            app_url,
            redirect: (canonical != url).then_some(canonical),
        })
    }
}

fn invalid(message: &str) -> TranslationError {
    TranslationError::Initialization(message.into())
}

fn safe_url(url: &str) -> Result<(), TranslationError> {
    if !url.starts_with('/')
        || url.starts_with("//")
        || url.bytes().any(|b| b == b'\\' || b.is_ascii_control())
    {
        return Err(invalid(
            "locale page URLs must be absolute same-origin paths",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(mode: RoutingMode) -> LocaleRoutes {
        LocaleRoutes::new(
            Locales::new(
                "en".parse().unwrap(),
                ["en", "fr", "zh-Hant"].map(|l| l.parse().unwrap()),
            )
            .unwrap(),
            mode,
        )
    }

    #[test]
    fn page_urls_beat_detection_and_only_first_root_visits_redirect() {
        let routes = routes(RoutingMode::PrefixExceptDefault);
        for (url, visited, expected, redirect) in [
            ("/pricing?x=1#buy", false, "en", None),
            ("/fr/pricing?x=1#buy", false, "fr", None),
            ("/", false, "fr", Some("/fr/")),
            ("/", true, "en", None),
            ("/en/", false, "en", Some("/")),
            (
                "/ZH-hant/pricing",
                true,
                "zh-Hant",
                Some("/zh-Hant/pricing"),
            ),
            ("/fr", true, "fr", Some("/fr/")),
            ("/de/pricing", false, "en", None),
        ] {
            let result = routes.resolve(url, Some("fr"), "zh-Hant", visited).unwrap();
            assert_eq!(result.locale.as_str(), expected, "{url}");
            assert_eq!(result.redirect.as_deref(), redirect, "{url}");
        }
        assert_eq!(
            routes
                .resolve("/", None, "zh-Hant", false)
                .unwrap()
                .locale
                .as_str(),
            "zh-Hant"
        );
    }

    #[test]
    fn modes_links_and_unsafe_inputs_preserve_the_url_contract() {
        let all = routes(RoutingMode::PrefixAll);
        assert_eq!(
            all.resolve("/pricing", Some("fr"), "fr", false)
                .unwrap()
                .redirect
                .as_deref(),
            Some("/en/pricing")
        );
        assert_eq!(
            all.href(&"fr".parse().unwrap(), "/en/a%2Fb?x=%26#go")
                .unwrap(),
            "/fr/a%2Fb?x=%26#go"
        );
        assert_eq!(all.app_url("/fr?q=1#two").unwrap(), "/?q=1#two");
        let none = routes(RoutingMode::None);
        let selected = none
            .resolve("/fr/pricing", Some("zh-Hant"), "en", true)
            .unwrap();
        assert_eq!(selected.locale.as_str(), "zh-Hant");
        assert_eq!(selected.app_url, "/fr/pricing");
        assert_eq!(selected.redirect, None);
        for bad in [
            "//evil.test",
            "https://evil.test",
            "/fr//evil.test",
            "/fr/\\evil",
            "/fr/\n",
        ] {
            assert!(all.href(&"en".parse().unwrap(), bad).is_err(), "{bad}");
        }
    }
}
