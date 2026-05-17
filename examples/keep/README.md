# Keep Example

Google Keep-style app showing Pocopine sync, Firebase client modules,
and an optional durable SQLite/OPFS local store in a real application
shape.

It demonstrates:

- Firebase-compatible sync by default, with a durable IndexedDB browser
  cache and a durable server-side SQLite stream,
- optional **durable browser local storage** through
  `pocopine-sync-sqlite` (OPFS-backed SQLite) when the app is run in
  cross-origin-isolated mode,
- a `KeepStore` `#[store]` that owns every durable UI field, paired
  with small components (`KeepBoard`, `KeepComposer`, `KeepNoteForm`,
  `KeepNoteBody`, `KeepNoteCard`, `KeepEditor`) and `pine-icons` for
  every glyph,
- optimistic sync pushes with live wake-up refreshes,
- a Firebase web SDK client module bundled through Pocopine's
  `.client.js` / `node_modules` path, adapted into a Pocopine auth
  extension service and `pocopine-auth-client::AuthSession`,
- a mixed note/checklist payload on one sync stream plus a separate
  synced tag stream for queryable labels. The sidebar registry is
  rebuilt from the tag stream and any labels already attached to
  hydrated notes, then missing labels are backfilled into the tag
  stream so refreshes do not lose the navigation list,
- a small memory-backed server stream so the example runs locally
  without database setup.

Run it with the CLI:

```bash
cargo run -p pocopine-cli -- dev --path examples/keep
```

The first run installs the example's local npm dependencies from
`package-lock.json` so `src/Firebase.client.js` can import the
Firebase SDK. Keep using `pocopine js --path examples/keep ...`
instead of direct npm scripts when adding client-module dependencies.

Open the app in two browser tabs. Creating, pinning, archiving, or
checking todo items in one tab wakes the other tab over SSE; the
data itself is fetched from `__pocopine/sync/v1/pull`. In the default
Firebase-friendly mode, browser rows and pending mutations survive
reloads through IndexedDB. In cross-origin-isolated mode, the app can
instead use the SQLite OPFS local store.

The Google login in this example keeps UI/state in Pocopine: the
Firebase client module only exposes provider calls, while the Keep
login component updates `pocopine-auth-client::AuthSession`. The local
sync backend still uses the same example streams for every signed-in
browser; a production app should verify Firebase ID tokens server-side
and scope rows by user.

By default the server leaves cross-origin isolation headers off so
Firebase Auth can load its hosted Google sign-in helper iframe; the
client sync plugin uses its IndexedDB local store in that mode. To test
the OPFS-isolated SQLite path, run with
`POCOPINE_KEEP_CROSS_ORIGIN_ISOLATED=1`; that restores
`Cross-Origin-Opener-Policy`, `Cross-Origin-Embedder-Policy`, and
`Cross-Origin-Resource-Policy`, but Firebase popup auth may be blocked
by the browser in that mode.

The server side uses a small rusqlite-backed stream source that stores
rows, row versions, incremental changes, and accepted mutation ids in
SQLite files under
`${TMPDIR}/pocopine_keep_notes.sqlite3` and
`${TMPDIR}/pocopine_keep_tags.sqlite3`. Restarting the bin rehydrates
from those databases, so notes and user-created tags survive
`cargo run` cycles and clients can keep using incremental cursors
across server restarts. Set `POCOPINE_KEEP_NOTES_DB_PATH` and
`POCOPINE_KEEP_TAGS_DB_PATH` when you want to isolate a dev or test
run from those default databases. Set
`POCOPINE_KEEP_ADDR=127.0.0.1:3023` when the default
`127.0.0.1:3022` development address is already in use.

The development guide in [`DEVELOPMENT.md`](./DEVELOPMENT.md) explains
the app-building process behind this example: state ownership,
component boundaries, shared form extraction, directive pitfalls, CSS
splitting, and browser verification.

Checks:

```bash
cargo test -p keep-example
cargo check -p keep-example --target wasm32-unknown-unknown
```
