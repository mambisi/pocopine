include!(concat!(env!("OUT_DIR"), "/pocopine_locale.rs"));

#[cfg(all(test, not(target_arch = "wasm32"), feature = "server-integration"))]
mod server_contract;

#[cfg(feature = "bad-argument")]
pub fn wrong_argument() -> String {
    t::cart::items("en".parse().unwrap(), "not a number")
}
#[cfg(feature = "missing-key")]
pub fn missing_key() -> String {
    t::common::unused("en".parse().unwrap())
}
#[cfg(all(target_arch = "wasm32", feature = "host-key-in-browser"))]
pub fn host_key() -> String {
    t::auth::denied("en".parse().unwrap())
}

// Export real generated call sites so release wasm cannot pass a bundle audit
// merely by dead-stripping the entire locale runtime.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn title(locale: &str) -> String {
    t::cart::title(locale.parse().unwrap())
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn items(locale: &str, count: u32) -> String {
    t::cart::items(locale.parse().unwrap(), u64::from(count).into())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod host_tests {
    use super::*;
    use pocopine_locale::{DateTimeArg, TimeZone};
    #[test]
    fn generated_worker_functions_use_explicit_locale_and_typed_inputs() {
        t::initialize().unwrap();
        assert_eq!(t::__runtime::title("en".parse().unwrap()), "Namespace");
        assert_eq!(t::std::title("en".parse().unwrap()), "Namespace");
        assert_eq!(
            t::cart::items("en".parse().unwrap(), 2u64.into()),
            "2 items"
        );
        assert_eq!(
            t::cart::items("fr".parse().unwrap(), 2u64.into()),
            "2 articles"
        );
        assert_eq!(t::auth::denied("en".parse().unwrap()), denial("en"));
        assert_eq!(t::auth::denied("fr".parse().unwrap()), denial("fr"));
        assert_eq!(
            t::r#type::r#match("en".parse().unwrap(), "locale arg", "self arg"),
            "Keyword self arg locale arg"
        );
        let at = DateTimeArg::new(0, TimeZone::utc()).unwrap();
        assert_eq!(
            t::schedule::when("fr".parse().unwrap(), at),
            "1 janvier 1970"
        );
    }

    fn denial(locale: &str) -> &'static str {
        match (cfg!(feature = "catalog-update"), locale) {
            (true, "en") => "Updated denial",
            (true, _) => "Accès refusé",
            (false, "en") => "HOST_COPY_SENTINEL",
            (false, _) => "Refusé",
        }
    }

    // The application owns durable message kinds. Translation IDs/build IDs
    // are deliberately absent: the worker uses its own generated functions.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Delivery {
        recipient: String,
        locale: pocopine_locale::Locale,
        message: MessageKind,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum MessageKind {
        AccessDenied,
        Appointment { at: DateTimeArg },
    }
    impl Delivery {
        fn render(&self) -> String {
            match &self.message {
                MessageKind::AccessDenied => t::auth::denied(self.locale.clone()),
                MessageKind::Appointment { at } => {
                    t::schedule::when(self.locale.clone(), at.clone())
                }
            }
        }
    }
    #[test]
    fn queued_recipient_jobs_survive_retries_and_catalog_deployments() {
        t::initialize().unwrap();
        // The exact same persisted inputs are consumed by both fixture builds.
        let jobs: Vec<Delivery> =
            serde_json::from_str(include_str!("queued-messages.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("OUT_DIR"),
            "/catalog-shape.json"
        )))
        .unwrap();
        assert_eq!(
            manifest["denied_id"],
            if cfg!(feature = "catalog-update") {
                5
            } else {
                4
            }
        );
        assert_eq!(manifest["build_id"], t::BUILD_ID);
        for job in jobs {
            let expected = match (&job.message, job.locale.as_str()) {
                (MessageKind::AccessDenied, locale) => denial(locale),
                (MessageKind::Appointment { .. }, _) => "1 mars 2024",
            };
            // Simulate a durable retry round-trip, not an in-memory copy.
            let persisted = serde_json::to_vec(&job).unwrap();
            for _ in 0..3 {
                let retry: Delivery = serde_json::from_slice(&persisted).unwrap();
                assert_eq!(retry.render(), expected, "recipient {}", retry.recipient);
            }
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser_tests {
    use super::*;
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn generated_browser_functions_wait_for_catalog_installation() {
        t::initialize(
            pocopine_locale::Locales::new(
                "en".parse().unwrap(),
                ["en", "fr"].map(|l| l.parse().unwrap()),
            )
            .unwrap(),
        )
        .unwrap();
        t::install(
            "en".parse().unwrap(),
            include_bytes!(concat!(env!("OUT_DIR"), "/en.json")),
        )
        .unwrap();
        let ui = pocopine_core::locale::client::LocaleController::new(
            t::catalogs().unwrap(),
            "en".parse().unwrap(),
        )
        .unwrap();
        ui.begin_switch("fr".parse().unwrap())
            .unwrap()
            .commit(Some(include_bytes!(concat!(env!("OUT_DIR"), "/fr.json"))))
            .unwrap();
        assert_eq!(items("fr", 2), "2 articles");
        assert_eq!(title("en"), "BROWSER_COPY_SENTINEL");
        assert_eq!(
            ui.error_message(
                &pocopine_core::ServerError::Network("private diagnostic".into()),
                t::cart::title
            ),
            "Titre"
        );
    }
}

#[cfg(feature = "template-integration")]
mod template_contract;
