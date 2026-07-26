# Pocopine website

The documentation site remains locally runnable with its `server` binary, but
its deployment contract is static and targets Cloudflare Pages.

```sh
# One-time: store a Cloudflare API token with Pages write access.
cargo run -p pocopine-cli -- deploy auth cf-pages

# One-time: store the RFC-100 R2 asset-sync credentials.
cargo run -p pocopine-cli -- assets auth

# Inspect the build/upload plan without writing or calling Cloudflare.
cargo run -p pocopine-cli -- deploy \
  --path examples/website \
  --target cf-pages \
  --prod \
  --dry-run

# Build the release WASM site and create a production Pages deployment.
cargo run -p pocopine-cli -- deploy \
  --path examples/website \
  --target cf-pages \
  --prod
```

CI can provide the token through `POCOPINE_CF_PAGES_TOKEN`, plus the R2
credentials through `POCOPINE_ASSETS_ACCESS_KEY_ID` and
`POCOPINE_ASSETS_SECRET_ACCESS_KEY`. The non-secret account, Pages project,
and production branch are declared in [`Cargo.toml`](Cargo.toml). A deploy
assembles `.pocopine/build/dist/`, promotes the generated content-hashed
`pkg/index.html` to the distribution root, uploads only missing assets, and
then creates the Pages deployment. Omit `--prod` to publish a preview
deployment.
