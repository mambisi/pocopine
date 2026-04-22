//! Canvas `measureText()` JS shim.
//!
//! On wasm32 this binds a tiny inline JS module that owns a shared
//! `OffscreenCanvas` (or `<canvas>` fallback for Safari ≤16) and
//! an LRU cache keyed by `font + \x01 + text`. The cache is on the
//! JS side so repeated measurements don't cross the WASM boundary.
//!
//! On host builds the JS is replaced with a deterministic
//! `chars() * 8.0` stub so unit tests can exercise the algorithm
//! without a browser.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(inline_js = r#"
let _ctx = null;
const _cache = new Map();
const _CACHE_MAX = 5000;

function _getCtx() {
    if (_ctx) return _ctx;
    try {
        const oc = new OffscreenCanvas(0, 0);
        _ctx = oc.getContext('2d');
    } catch (_) {}
    if (!_ctx && typeof document !== 'undefined') {
        const c = document.createElement('canvas');
        _ctx = c.getContext('2d');
    }
    return _ctx;
}

export function measure_text(font, text) {
    const key = font + '\x01' + text;
    const hit = _cache.get(key);
    if (hit !== undefined) {
        _cache.delete(key);
        _cache.set(key, hit);
        return hit;
    }
    const ctx = _getCtx();
    if (!ctx) return text.length * 8;
    if (ctx.font !== font) ctx.font = font;
    const w = ctx.measureText(text).width;
    if (_cache.size >= _CACHE_MAX) {
        const first = _cache.keys().next().value;
        if (first !== undefined) _cache.delete(first);
    }
    _cache.set(key, w);
    return w;
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = measure_text)]
    fn measure_text_js(font: &str, text: &str) -> f64;
}

#[cfg(target_arch = "wasm32")]
#[inline]
pub fn measure_text(font: &str, text: &str) -> f64 {
    measure_text_js(font, text)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn measure_text(_font: &str, text: &str) -> f64 {
    text.chars().count() as f64 * 8.0
}
