//! RFC-099 Phase 4 — host SSR renderer for the website.
//!
//! Registers the full app (the same `boot_app()` the wasm client uses)
//! and renders a URL to the `[pp-app]` document the client claims via
//! `App::hydrate`. Prints the document to stdout (a build/dev step can
//! splice it into `index.html`).
//!
//! ```sh
//! cargo run -p website --bin website-ssr -- /
//! ```
//!
//! What server-renders today: the shell (WebsiteApp) + static child
//! components + the matched route's static structure, with each scope's
//! `data-pp-state` island. NOT yet (degrade to client mount/fill):
//! slotted components (pine compounds, command palette), and any state
//! derived in `on_setup` (the host doesn't run setup) — those hydrate /
//! fill in on the client.

use website::{boot_app, WebsiteApp};

fn main() {
    // URL to render (default "/").
    let url = std::env::args().nth(1).unwrap_or_else(|| "/".to_string());

    // Register every component + route (side effects); the returned App
    // is only needed for those registrations server-side.
    let _app = boot_app();

    let page = pocopine_ssr::render_app_to_string(&WebsiteApp::default(), &url)
        .expect("WebsiteApp is registered");

    // RFC-099 — every registered component's CSS, so the SSR'd page paints
    // fully styled before the wasm bundle boots (no FOUC). Placed inside
    // `[pp-app]` but BEFORE `<website-app>`, so it doesn't shift the app
    // root's node paths; the client `inject_style` dedups against it.
    let styles = pocopine_ssr::collected_style_tags();

    // The [pp-app] custom-element-host document shape App::hydrate claims.
    let doc = format!(
        "<div pp-app>{styles}<website-app>{}{}</website-app></div>",
        page.body,
        page.state_island()
    );
    println!("{doc}");
}
