use pocopine::ServerResult;

pocopine::locale::include_translations!();

#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;

#[pocopine::server(public)]
pub async fn welcome(
    locale: pocopine_server::Extension<pocopine::locale::Locale>,
    name: String,
) -> ServerResult<String> {
    Ok(t::common::welcome(locale.0, &name))
}

#[pocopine::server(public)]
pub async fn denied(
    locale: pocopine_server::Extension<pocopine::locale::Locale>,
) -> ServerResult<String> {
    Err(pocopine::ServerError::forbidden(t::common::forbidden(
        locale.0,
    )))
}
