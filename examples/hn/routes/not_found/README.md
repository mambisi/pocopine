# hn-route-not-found

Static HN fallback route using the RFC 067 route-crate entry point.

The current split builder can lower this route crate to descriptor JS
because the template is static. Dynamic HN routes (`home` and `story`)
stay in `src/routes` until compiled-plan and handler ABI lowering land.

