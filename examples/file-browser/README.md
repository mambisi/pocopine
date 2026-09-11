# Pocopine Storage Browser

Local MinIO:

```bash
docker compose -f examples/file-browser/docker-compose.yml up -d
pocopine dev --path examples/file-browser
```

The connection modal's MinIO defaults target:

- endpoint: `http://127.0.0.1:9000`
- access key: `minioadmin`
- secret key: `minioadmin`
- bucket: `pocopine-demo`

The same connection dialog can also save Google Cloud Storage profiles.
Leave the endpoint empty to use Google's default endpoint and application
default credentials, or set an emulator endpoint such as
`http://127.0.0.1:4443` with anonymous auth for a local GCS JSON API
emulator.

Connection profiles are stored locally at
`examples/file-browser/.data/storage-browser/connections.json` unless
`POCOPINE_STORAGE_BROWSER_CONFIG` is set.

Uploads use Pine's existing `pine-upload-root` compound and
`pocopine_storage::UploadClient`. The server mounts
`pocopine-storage-s3` or `pocopine-storage-gcs` behind that upload
endpoint, and the active connection id plus virtual prefix are passed as
upload metadata.

## Dialog Centering

`pine-dialog-content` uses Pine's default `fade-scale` transition, and
that transition owns the rendered panel's `transform`. Pine's
layout-bearing custom-element hosts default to `display: contents`, so
center the rendered `.pine-dialog-portal` root with flex or grid, then
style `.pine-dialog-content` as the card. Do not center the animated
panel with `transform: translate(-50%, -50%)`, because the animation
atom replaces that transform while the dialog opens and closes.

## State changes and derived values

Input actions accept child `pp:update:*` events when a change must also
update another field or start a request. The view filter writes directly
to the store, the command palette loads when its opening action runs, and
the configuration form clamps both size settings in one action.

The size control derives its amount, bounds, labels, and range style with
`#[computed]`. Its only watcher takes current numeric values and a borrowed
unit string, synchronizing native input DOM properties without retaining
previous values. Incoming bounds do not publish a model change; the parent
owns normalization of its stored value.

Opening a dialog starts a keyed editing session. `on_mount` initializes
its draft; reopening or selecting another connection replaces the session.
Closed sessions remain mounted long enough for Pine's exit animation.
The permanent shell owns the new-folder keyboard shortcut.

See [Migrating mutable watchers](../../docs/guides/reactivity/06-readonly-watch-migration.md)
for the complete patterns and the remaining library migration status.
