//! Module-worker transport. Grammars stay inside the worker's WASM instance.
use super::runtime::Runtime;
use crate::{
    LanguageRegistry,
    syntax::{SyntaxParser, SyntaxRequest, SyntaxResponse},
};
use std::{cell::RefCell, collections::BTreeMap, rc::Weak};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{DedicatedWorkerGlobalScope, ErrorEvent, MessageEvent, Worker};

#[derive(Clone)]
pub struct LanguageWorkerConfig {
    pub module_url: String,
    pub entrypoint: String,
}
impl LanguageWorkerConfig {
    /// The module is the app's content-hashed wasm-bindgen JS glue; entrypoint
    /// is an exported Rust function calling `start_language_worker`.
    pub fn new(module_url: impl Into<String>, entrypoint: impl Into<String>) -> Self {
        Self {
            module_url: module_url.into(),
            entrypoint: entrypoint.into(),
        }
    }
}
#[derive(Clone)]
pub(super) struct Settings {
    pub config: LanguageWorkerConfig,
    pub indents: BTreeMap<String, String>,
}
thread_local! {
    static SETTINGS: RefCell<Option<Settings>> = const { RefCell::new(None) };
    static SERVICE: RefCell<Option<WorkerService>> = const { RefCell::new(None) };
}
/// Configure before mounting editors. Definitions are metadata here; query
/// compilation and parsing happen in the worker's independently built registry.
/// Keep the same registry factory in the main and worker entrypoints.
pub fn configure_languages(
    config: LanguageWorkerConfig,
    languages: &LanguageRegistry,
) -> Result<(), JsValue> {
    let window = web_sys::window()
        .ok_or_else(|| JsValue::from_str("configure_languages requires a window"))?;
    let base = window.location().href()?;
    let url = web_sys::Url::new_with_base(&config.module_url, &base)?;
    if !matches!(url.protocol().as_str(), "http:" | "https:") || config.entrypoint.is_empty() {
        return Err(JsValue::from_str(
            "language worker requires an HTTP(S) module URL and export name",
        ));
    }
    let config = LanguageWorkerConfig {
        module_url: url.href(),
        ..config
    };
    SETTINGS.with(|settings| {
        *settings.borrow_mut() = Some(Settings {
            config,
            indents: languages
                .iter()
                .map(|language| (language.id.clone(), language.indent.clone()))
                .collect(),
        })
    });
    Ok(())
}
pub(super) fn settings() -> Option<Settings> {
    SETTINGS.with(|s| s.borrow().clone())
}
pub(super) fn indentation(language: &str) -> String {
    SETTINGS
        .with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|s| s.indents.get(language))
                .cloned()
        })
        .unwrap_or_else(|| if language == "rust" { "    " } else { "  " }.into())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) enum WorkerEvent {
    Ready,
    Result(SyntaxResponse),
    Failed(String),
}
struct WorkerService {
    global: DedicatedWorkerGlobalScope,
    _callback: Closure<dyn FnMut(MessageEvent)>,
}
impl Drop for WorkerService {
    fn drop(&mut self) {
        self.global.set_onmessage(None);
    }
}
/// Call from a wasm-bindgen export in a module worker, never from app startup.
/// Calling twice replaces and drops the previous parser and handler.
pub fn start_language_worker(languages: LanguageRegistry) -> Result<(), JsValue> {
    let global: DedicatedWorkerGlobalScope = js_sys::global()
        .dyn_into()
        .map_err(|_| JsValue::from_str("start_language_worker requires a dedicated worker"))?;
    let destination = global.clone();
    let mut parser = SyntaxParser::new(languages);
    let callback = Closure::wrap(Box::new(move |event: MessageEvent| {
        let message = event
            .data()
            .as_string()
            .ok_or_else(|| "invalid worker message".to_string())
            .and_then(|data| {
                serde_json::from_str::<SyntaxRequest>(&data).map_err(|e| e.to_string())
            });
        let response = match message {
            Ok(request) => WorkerEvent::Result(parser.process(request)),
            Err(error) => WorkerEvent::Failed(error),
        };
        if let Ok(data) = serde_json::to_string(&response) {
            let _ = destination.post_message(&JsValue::from_str(&data));
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    SERVICE.with(|service| {
        service.borrow_mut().take();
        global.set_onmessage(Some(callback.as_ref().unchecked_ref()));
        *service.borrow_mut() = Some(WorkerService {
            global: global.clone(),
            _callback: callback,
        });
    });
    global.post_message(&JsValue::from_str("\"Ready\""))
}

pub(super) struct WorkerClient {
    worker: Worker,
    url: String,
    pub ready: bool,
    timer: Option<(i32, Closure<dyn FnMut()>)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _error: Closure<dyn FnMut(ErrorEvent)>,
}
impl WorkerClient {
    pub fn new(config: &LanguageWorkerConfig, runtime: &Weak<Runtime>) -> Result<Self, JsValue> {
        let module = serde_json::to_string(&config.module_url).unwrap();
        let entrypoint = serde_json::to_string(&config.entrypoint).unwrap();
        let script = format!(
            "try {{ const m = await import({module}); await m.default(); m[{entrypoint}](); }} catch(e) {{ postMessage(JSON.stringify({{Failed: String(e)}})); }}"
        );
        let parts = js_sys::Array::of1(&JsValue::from_str(&script));
        let properties = web_sys::BlobPropertyBag::new();
        properties.set_type("text/javascript");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &properties)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        let options = web_sys::WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);
        let worker = match Worker::new_with_options(&url, &options) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = web_sys::Url::revoke_object_url(&url);
                return Err(error);
            }
        };
        let weak = runtime.clone();
        let message = Closure::wrap(Box::new(move |event: MessageEvent| {
            if let Some(runtime) = weak.upgrade() {
                let data = event
                    .data()
                    .as_string()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| WorkerEvent::Failed("invalid worker response".into()));
                runtime.receive_syntax(data);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        let weak = runtime.clone();
        let error = Closure::wrap(Box::new(move |event: ErrorEvent| {
            event.prevent_default();
            if let Some(runtime) = weak.upgrade() {
                runtime.receive_syntax(WorkerEvent::Failed(event.message()));
            }
        }) as Box<dyn FnMut(ErrorEvent)>);
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(error.as_ref().unchecked_ref()));
        let mut result = Self {
            worker,
            url,
            ready: false,
            timer: None,
            _message: message,
            _error: error,
        };
        result.arm_timeout(runtime, 30_000)?;
        Ok(result)
    }
    fn arm_timeout(&mut self, runtime: &Weak<Runtime>, ms: i32) -> Result<(), JsValue> {
        self.clear_timeout();
        let weak = runtime.clone();
        let callback = Closure::wrap(Box::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.receive_syntax(WorkerEvent::Failed("language worker timed out".into()));
            }
        }) as Box<dyn FnMut()>);
        let id = web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                ms,
            )?;
        self.timer = Some((id, callback));
        Ok(())
    }
    pub fn clear_timeout(&mut self) {
        if let Some((id, _)) = self.timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(id);
        }
    }
    pub fn mark_ready(&mut self) {
        self.ready = true;
        self.clear_timeout();
        let _ = web_sys::Url::revoke_object_url(&self.url);
    }
    pub fn send(
        &mut self,
        request: &SyntaxRequest,
        runtime: &Weak<Runtime>,
    ) -> Result<(), JsValue> {
        self.arm_timeout(runtime, 10_000)?;
        self.worker.post_message(&JsValue::from_str(
            &serde_json::to_string(request).map_err(|e| JsValue::from_str(&e.to_string()))?,
        ))
    }
}
impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.clear_timeout();
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.terminate();
        let _ = web_sys::Url::revoke_object_url(&self.url);
    }
}
