use crate::t;
use pocopine::locale::Locale;
use pocopine_server::{
    Server,
    axum::Router,
    locale::{FrameworkMessages, ServerLocale},
    static_files,
};

/// Workers snapshot the recipient's language from durable semantic job data.
pub fn recipient_message(locale: Locale, name: &str) -> String {
    t::common::recipient(locale, name)
}

pub async fn run() -> std::io::Result<()> {
    t::initialize().map_err(std::io::Error::other)?;
    let messages = FrameworkMessages {
        unauthorized: t::common::unauthorized,
        forbidden: t::common::forbidden,
        bad_request: t::common::bad_request,
        internal: t::common::internal,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // Publish only built browser artifacts; source catalogs and generated host
    // Rust remain private even when a visitor guesses their filesystem paths.
    let router = Router::new()
        .nest_service("/pkg", static_files(root.join("pkg")))
        .route_service(
            "/",
            pocopine_server::tower_http::services::ServeFile::new(pocopine_server::index_file(
                root,
            )),
        );
    let port = std::env::var("PORT").unwrap_or_else(|_| "3088".into());
    Server::new(router)
        .with_locale(ServerLocale::new(t::locales(), messages))
        .serve(&format!("127.0.0.1:{port}"))
        .await
}
