use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-demo", template = poco! {
    <section>
      <nav aria-label="Language">
        <button @click="language('en')" lang="en">English</button>
        <button @click="language('fr')" lang="fr">Français</button>
        <button @click="language('ar')" lang="ar">العربية</button>
      </nav>
      <nav aria-label="Pages">
        <a pp-route :href="home_href" pp-text="$t.common.home"></a>
        <a pp-route :href="pricing_href" pp-text="$t.common.pricing"></a>
      </nav>
      <h1 pp-text="$t('common.welcome', name)"></h1>
      <label>
        <span pp-text="$t.common.name"></span>
        <input pp-model="name" :placeholder="$t.common.name" />
      </label>
      <p pp-text="$t('common.items', count)"></p>
      <button @click="increment" pp-text="$t.common.add"></button>
      <button @click="greet" pp-text="$t.common.server"></button>
      <button @click="reject" pp-text="$t.common.error"></button>
      <p role="status" pp-text="status"></p>
      <p pp-text="$t.common.more"><a href="https://pocopine.dev"></a></p>
      <pp-outlet></pp-outlet>
    </section>
})]
struct LocaleDemo {
    name: String,
    count: u32,
    status: String,
}

#[handlers]
impl LocaleDemo {
    #[computed]
    fn home_href() -> String {
        pocopine::locale::client::active()
            .expect("locale boot")
            .href("/")
            .expect("page URL")
    }
    #[computed]
    fn pricing_href() -> String {
        pocopine::locale::client::active()
            .expect("locale boot")
            .href("/pricing")
            .expect("page URL")
    }
    fn on_mount(&mut self) {
        self.name = "Amina".into();
        self.count = 1;
    }
    fn increment(&mut self) {
        self.count += 1;
    }
    fn language(&mut self, tag: String) {
        dispatch!(
            async move {
                let locale = tag
                    .parse()
                    .map_err(|e: pocopine::locale::InvalidLocale| e.to_string())?;
                pocopine::locale::client::active()
                    .map_err(|e| e.to_string())?
                    .set_locale(locale)
                    .await
                    .map_err(|e| e.to_string())
            }
            .await,
            |state, result| {
                state.status = if result.is_err() {
                    let ui = pocopine::locale::client::active().expect("locale boot");
                    crate::t::common::network(ui.snapshot())
                } else {
                    String::new()
                };
            }
        );
    }
    fn greet(&mut self) {
        let name = self.name.clone();
        dispatch!(crate::welcome(name).await, |state, result| {
            state.show(result);
        });
    }
    fn reject(&mut self) {
        dispatch!(crate::denied().await, |state, result| {
            state.show(result);
        });
    }
}

impl LocaleDemo {
    fn show(&mut self, result: pocopine::ServerResult<String>) {
        self.status = result.unwrap_or_else(|error| {
            pocopine::locale::client::active()
                .expect("locale boot")
                .error_message(&error, crate::t::common::network)
        });
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(name = "locale-page", template = poco! {
    <p id="route-page" pp-text="$route.path"></p>
})]
struct LocalePage {}

#[handlers]
impl LocalePage {}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub async fn main() {
    if let Err(error) = bootstrap().await {
        // The loader already provides a visible reload action. Preserve the
        // diagnostic without throwing out of wasm's startup task.
        web_sys::console::error_1(&error.into());
    }
}

async fn bootstrap() -> Result<(), String> {
    crate::t::initialize(crate::t::locales()).map_err(|e| e.to_string())?;
    pocopine::locale::client::boot(crate::t::catalogs().map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;
    App::new()
        .register::<LocaleDemo>()
        .route::<LocalePage>("/")
        .route::<LocalePage>("/pricing")
        .run();
    Ok(())
}
