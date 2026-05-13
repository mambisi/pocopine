//! Typed browser storage helpers.
//!
//! This module is intentionally small: it wraps the browser
//! `localStorage` surface with serde so app preferences can be stored
//! as real Rust types instead of ad hoc strings. On non-wasm targets,
//! storage methods return [`StorageError::Unavailable`].

use std::fmt;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Error returned by typed browser storage operations.
#[derive(Debug)]
pub enum StorageError {
    /// The requested browser storage surface is not available. This is
    /// expected on host targets and can also happen in restricted
    /// browser contexts.
    Unavailable,
    /// The value could not be serialized to JSON.
    Serialize(serde_json::Error),
    /// The stored JSON could not be deserialized as the requested type.
    Deserialize(serde_json::Error),
    /// Browser storage returned an error while reading, writing, or
    /// removing an item.
    Browser(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("browser localStorage is unavailable"),
            Self::Serialize(err) => write!(f, "could not serialize localStorage value: {err}"),
            Self::Deserialize(err) => {
                write!(f, "could not deserialize localStorage value: {err}")
            }
            Self::Browser(err) => write!(f, "browser localStorage error: {err}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(err) | Self::Deserialize(err) => Some(err),
            Self::Unavailable | Self::Browser(_) => None,
        }
    }
}

/// Typed access to `window.localStorage` for one key.
///
/// Values are encoded as JSON using `serde_json`, so enum/string
/// preferences remain readable in devtools while still round-tripping
/// through the requested Rust type.
#[derive(Clone, Debug)]
pub struct LocalStorage<T> {
    key: String,
    _marker: PhantomData<fn() -> T>,
}

impl<T> LocalStorage<T> {
    /// Create a typed handle for `key`.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            _marker: PhantomData,
        }
    }

    /// The underlying `localStorage` key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Remove the stored value for this key.
    pub fn remove(&self) -> Result<(), StorageError> {
        remove_local_storage_item(&self.key)
    }
}

impl<T> LocalStorage<T>
where
    T: DeserializeOwned,
{
    /// Load and deserialize the stored value.
    ///
    /// Returns `Ok(None)` when the key is absent. Malformed JSON or a
    /// type mismatch returns [`StorageError::Deserialize`] instead of
    /// silently falling back.
    pub fn get(&self) -> Result<Option<T>, StorageError> {
        let Some(value) = get_local_storage_item(&self.key)? else {
            return Ok(None);
        };
        serde_json::from_str(&value)
            .map(Some)
            .map_err(StorageError::Deserialize)
    }
}

impl<T> LocalStorage<T>
where
    T: Serialize,
{
    /// Serialize and save `value`.
    pub fn set(&self, value: &T) -> Result<(), StorageError> {
        let value = serde_json::to_string(value).map_err(StorageError::Serialize)?;
        set_local_storage_item(&self.key, &value)
    }
}

#[cfg(target_arch = "wasm32")]
fn get_local_storage_item(key: &str) -> Result<Option<String>, StorageError> {
    storage()?
        .get_item(key)
        .map_err(|err| StorageError::Browser(js_error_message(err)))
}

#[cfg(not(target_arch = "wasm32"))]
fn get_local_storage_item(_: &str) -> Result<Option<String>, StorageError> {
    Err(StorageError::Unavailable)
}

#[cfg(target_arch = "wasm32")]
fn set_local_storage_item(key: &str, value: &str) -> Result<(), StorageError> {
    storage()?
        .set_item(key, value)
        .map_err(|err| StorageError::Browser(js_error_message(err)))
}

#[cfg(not(target_arch = "wasm32"))]
fn set_local_storage_item(_: &str, _: &str) -> Result<(), StorageError> {
    Err(StorageError::Unavailable)
}

#[cfg(target_arch = "wasm32")]
fn remove_local_storage_item(key: &str) -> Result<(), StorageError> {
    storage()?
        .remove_item(key)
        .map_err(|err| StorageError::Browser(js_error_message(err)))
}

#[cfg(not(target_arch = "wasm32"))]
fn remove_local_storage_item(_: &str) -> Result<(), StorageError> {
    Err(StorageError::Unavailable)
}

#[cfg(target_arch = "wasm32")]
fn storage() -> Result<web_sys::Storage, StorageError> {
    let window = web_sys::window().ok_or(StorageError::Unavailable)?;
    window
        .local_storage()
        .map_err(|err| StorageError::Browser(js_error_message(err)))?
        .ok_or(StorageError::Unavailable)
}

#[cfg(target_arch = "wasm32")]
fn js_error_message(err: wasm_bindgen::JsValue) -> String {
    err.as_string().unwrap_or_else(|| format!("{err:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn local_storage_reports_unavailable_on_host() {
        let storage = LocalStorage::<String>::new("pocopine.test");
        assert!(matches!(storage.get(), Err(StorageError::Unavailable)));
        assert!(matches!(
            storage.set(&"value".to_string()),
            Err(StorageError::Unavailable)
        ));
        assert!(matches!(storage.remove(), Err(StorageError::Unavailable)));
    }
}
