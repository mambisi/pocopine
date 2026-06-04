---
title: "Typed browser storage"
description: "Typed localStorage helpers for small browser preferences, with the auth-token and SSR boundaries."
---

# Typed browser storage

Use `pocopine::storage::LocalStorage<T>` for small, non-sensitive
browser preferences that should survive a reload: theme, density, a
remembered tab, or a dismissed hint.

`LocalStorage<T>` is typed through serde. Values are written as JSON,
and reads fail with `StorageError::Deserialize` if the stored value no
longer matches the requested Rust type.

```rust
use pocopine::storage::LocalStorage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Theme {
    #[default]
    Light,
    Dark,
}

const THEME_KEY: &str = "my_app.theme";

fn load_theme() -> Theme {
    LocalStorage::<Theme>::new(THEME_KEY)
        .get()
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn save_theme(theme: Theme) {
    let _ = LocalStorage::new(THEME_KEY).set(&theme);
}
```

## Failure model

All operations return `Result<_, StorageError>`.

- On host targets, storage is unavailable.
- In private or restricted browser contexts, storage may be unavailable.
- Quota, permission, and browser errors are returned as
  `StorageError::Browser`.
- Malformed JSON is returned as `StorageError::Deserialize`; it is not
  silently ignored by the storage helper.

Apps can decide whether a failed preference read should show an error,
clear the key, or fall back to a default.

## Security boundary

Do not store access tokens, refresh tokens, secrets, or authorization
claims here. JavaScript-readable browser storage is readable by any XSS
bug in the same origin. For auth token persistence, use
`pocopine-auth-client`'s `TokenStorage` contract and prefer an
`httpOnly` cookie for high-value applications.

## SSR and hydration

Reading `localStorage` during an initial render can make server-rendered
HTML differ from the browser's first render. For SSR-compatible
components, read preferences in `on_mount` or render a server-stable
default and then update after mount. Client-only examples can read during
store construction when avoiding a first-frame flash is more important.
