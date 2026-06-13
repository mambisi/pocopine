//! Low-level access to CLI-bundled `.client.ts` modules.
//!
//! The CLI writes default exports into `window.__pp_client_modules`.
//! The public author-facing API is `#[pocopine::client_module]`;
//! this wrapper keeps generated facades away from raw `Reflect`,
//! `Promise`, and callback-lifetime plumbing.

use std::error::Error;
use std::fmt;

use serde::de::DeserializeOwned;

use crate::ScopeId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientModuleError {
    message: String,
}

impl ClientModuleError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ClientModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ClientModuleError {}

#[derive(Clone, Debug)]
pub struct ClientModule {
    name: String,
    #[cfg(target_arch = "wasm32")]
    value: wasm_bindgen::JsValue,
}

impl ClientModule {
    pub fn required(name: impl Into<String>) -> Result<Self, ClientModuleError> {
        let name = name.into();
        #[cfg(not(target_arch = "wasm32"))]
        {
            Err(ClientModuleError::new(format!(
                "client module `{name}` is only available in the browser"
            )))
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self::optional(&name)?.ok_or_else(|| {
                ClientModuleError::new(format!("client module `{name}` is not registered"))
            })
        }
    }

    pub fn optional(name: impl Into<String>) -> Result<Option<Self>, ClientModuleError> {
        let name = name.into();
        optional_impl(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn call_async<T>(&self, method: impl AsRef<str>) -> Result<T, ClientModuleError>
    where
        T: DeserializeOwned,
    {
        call_async_impl(self, method.as_ref()).await
    }

    pub fn subscribe<T>(
        &self,
        scope: ScopeId,
        method: impl AsRef<str>,
        handler: impl FnMut(Result<T, ClientModuleError>) + 'static,
    ) -> Result<(), ClientModuleError>
    where
        T: DeserializeOwned + 'static,
    {
        subscribe_impl(self, scope, method.as_ref(), handler)
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use js_sys::{Function, Promise, Reflect};
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::{JsFuture, spawn_local};

    use super::{ClientModule, ClientModuleError, DeserializeOwned, ScopeId};

    const REGISTRY_NAME: &str = "__pp_client_modules";

    thread_local! {
        static SUBSCRIPTIONS: RefCell<HashMap<ScopeId, Vec<ClientModuleSubscription>>> =
            RefCell::new(HashMap::new());
    }

    struct ClientModuleSubscription {
        unsubscribe: Option<Function>,
        _callback: Closure<dyn FnMut(JsValue)>,
    }

    impl Drop for ClientModuleSubscription {
        fn drop(&mut self) {
            if let Some(unsubscribe) = self.unsubscribe.take() {
                let _ = unsubscribe.call0(&JsValue::NULL);
            }
        }
    }

    pub(super) fn optional_impl(name: String) -> Result<Option<ClientModule>, ClientModuleError> {
        let global = js_sys::global();
        let registry =
            Reflect::get(&global, &JsValue::from_str(REGISTRY_NAME)).map_err(|value| {
                ClientModuleError::new(js_value_message(value, "read client modules"))
            })?;
        if registry.is_null() || registry.is_undefined() {
            return Ok(None);
        }

        let value = Reflect::get(&registry, &JsValue::from_str(&name)).map_err(|value| {
            ClientModuleError::new(format!(
                "client module `{name}` could not be read: {}",
                js_value_message(value, "unknown JavaScript error")
            ))
        })?;
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }

        Ok(Some(ClientModule { name, value }))
    }

    pub(super) async fn call_async_impl<T>(
        module: &ClientModule,
        method: &str,
    ) -> Result<T, ClientModuleError>
    where
        T: DeserializeOwned,
    {
        let promise = module
            .call_raw(method)?
            .dyn_into::<Promise>()
            .map_err(|_| {
                ClientModuleError::new(format!(
                    "client module `{}.{method}` did not return a Promise",
                    module.name
                ))
            })?;
        let value = JsFuture::from(promise).await.map_err(|value| {
            ClientModuleError::new(format!(
                "client module `{}.{method}` failed: {}",
                module.name,
                js_value_message(value, "unknown JavaScript error")
            ))
        })?;
        decode_value(&module.name, method, value)
    }

    pub(super) fn subscribe_impl<T>(
        module: &ClientModule,
        scope: ScopeId,
        method: &str,
        handler: impl FnMut(Result<T, ClientModuleError>) + 'static,
    ) -> Result<(), ClientModuleError>
    where
        T: DeserializeOwned + 'static,
    {
        let method_fn = module.method(method)?;
        let module_name = module.name.clone();
        let method_name = method.to_string();
        let handler = Rc::new(RefCell::new(handler));
        let callback = Closure::<dyn FnMut(JsValue)>::new(move |value| {
            let result = decode_value(&module_name, &method_name, value);
            let handler = handler.clone();
            spawn_local(async move {
                handler.borrow_mut()(result);
            });
        });
        let unsubscribe = method_fn
            .call1(&module.value, callback.as_ref())
            .map_err(|value| {
                ClientModuleError::new(format!(
                    "client module `{}.{method}` subscribe failed: {}",
                    module.name,
                    js_value_message(value, "unknown JavaScript error")
                ))
            })?
            .dyn_into::<Function>()
            .map_err(|_| {
                ClientModuleError::new(format!(
                    "client module `{}.{method}` did not return an unsubscribe function",
                    module.name
                ))
            })?;

        SUBSCRIPTIONS.with(|subscriptions| {
            subscriptions
                .borrow_mut()
                .entry(scope)
                .or_default()
                .push(ClientModuleSubscription {
                    unsubscribe: Some(unsubscribe),
                    _callback: callback,
                });
        });
        crate::on_scope_unmount_for(scope, move || {
            SUBSCRIPTIONS.with(|subscriptions| {
                subscriptions.borrow_mut().remove(&scope);
            });
        });

        Ok(())
    }

    impl ClientModule {
        fn call_raw(&self, method: &str) -> Result<JsValue, ClientModuleError> {
            self.method(method)?.call0(&self.value).map_err(|value| {
                ClientModuleError::new(format!(
                    "client module `{}.{method}` failed: {}",
                    self.name,
                    js_value_message(value, "unknown JavaScript error")
                ))
            })
        }

        fn method(&self, method: &str) -> Result<Function, ClientModuleError> {
            Reflect::get(&self.value, &JsValue::from_str(method))
                .map_err(|value| {
                    ClientModuleError::new(format!(
                        "client module `{}.{method}` could not be read: {}",
                        self.name,
                        js_value_message(value, "unknown JavaScript error")
                    ))
                })?
                .dyn_into::<Function>()
                .map_err(|_| {
                    ClientModuleError::new(format!(
                        "client module `{}.{method}` is not a function",
                        self.name
                    ))
                })
        }
    }

    fn decode_value<T>(module: &str, method: &str, value: JsValue) -> Result<T, ClientModuleError>
    where
        T: DeserializeOwned,
    {
        serde_wasm_bindgen::from_value(value).map_err(|err| {
            ClientModuleError::new(format!(
                "client module `{module}.{method}` returned an invalid payload: {err}"
            ))
        })
    }

    fn js_value_message(value: JsValue, fallback: &str) -> String {
        if let Some(message) = value.as_string() {
            return message;
        }
        if let Ok(message) = Reflect::get(&value, &JsValue::from_str("message"))
            && let Some(message) = message.as_string()
        {
            return message;
        }
        fallback.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
use wasm::{call_async_impl, optional_impl, subscribe_impl};

#[cfg(not(target_arch = "wasm32"))]
fn optional_impl(name: String) -> Result<Option<ClientModule>, ClientModuleError> {
    let _ = name;
    Ok(None)
}

#[cfg(not(target_arch = "wasm32"))]
async fn call_async_impl<T>(module: &ClientModule, method: &str) -> Result<T, ClientModuleError>
where
    T: DeserializeOwned,
{
    Err(ClientModuleError::new(format!(
        "client module `{}.{method}` is only available in the browser",
        module.name
    )))
}

#[cfg(not(target_arch = "wasm32"))]
fn subscribe_impl<T>(
    module: &ClientModule,
    _scope: ScopeId,
    method: &str,
    _handler: impl FnMut(Result<T, ClientModuleError>) + 'static,
) -> Result<(), ClientModuleError>
where
    T: DeserializeOwned + 'static,
{
    Err(ClientModuleError::new(format!(
        "client module `{}.{method}` is only available in the browser",
        module.name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_client_module_message_names_the_module() {
        let err = ClientModule::required("firebase").unwrap_err();
        assert_eq!(
            err.message(),
            "client module `firebase` is only available in the browser"
        );
    }
}
