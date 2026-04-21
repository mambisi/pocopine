//! Thin Web Animations API wrapper — `Element.animate(keyframes,
//! options)` as a Rust function returning a cancelable future.
//!
//! This is the "escape hatch" programmatic API: use it when the
//! declarative preset catalogue in [`crate::animate::presets`] isn't
//! enough, or to drive imperative motion like the FLIP helper in
//! `flip.rs`.

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::Element;

/// One entry in a WAAPI keyframe list — a bundle of CSS property
/// names → values at a given offset (0..=1). pocopine keeps this
/// minimal: pairs of `(property, value)` strings.
#[derive(Clone, Debug)]
pub struct Keyframe {
    pub props: Vec<(&'static str, String)>,
}

impl Keyframe {
    /// Construct a keyframe from an iterator of `(prop, value)`.
    pub fn from_iter<I, V>(iter: I) -> Self
    where
        I: IntoIterator<Item = (&'static str, V)>,
        V: Into<String>,
    {
        Self {
            props: iter.into_iter().map(|(k, v)| (k, v.into())).collect(),
        }
    }
}

/// Playback options for [`animate`] — maps onto the
/// `EffectTiming`/`KeyframeEffectOptions` dictionary in WAAPI.
#[derive(Clone, Debug)]
pub struct AnimateOptions {
    /// Animation duration in milliseconds. Default `200`.
    pub duration_ms: f64,
    /// CSS easing string — `"linear"`, `"ease-out"`, or a
    /// `"cubic-bezier(…)"`. Default `"cubic-bezier(0, 0, 0.2, 1)"`
    /// (ease-out).
    pub easing: &'static str,
    /// Pre-delay in ms. Default `0`.
    pub delay_ms: f64,
    /// What to do once the animation finishes — one of `"none"`,
    /// `"forwards"`, `"backwards"`, `"both"`. Default `"forwards"`
    /// so the final keyframe state sticks.
    pub fill: &'static str,
}

impl Default for AnimateOptions {
    fn default() -> Self {
        Self {
            duration_ms: 200.0,
            easing: "cubic-bezier(0, 0, 0.2, 1)",
            delay_ms: 0.0,
            fill: "forwards",
        }
    }
}

/// Handle to a running animation. Drop it and the animation keeps
/// running (with `fill: "forwards"` the final state persists). Call
/// [`AnimationHandle::cancel`] to interrupt. `.finished()` returns a
/// Promise you can await.
pub struct AnimationHandle {
    inner: web_sys::Animation,
}

impl AnimationHandle {
    /// Cancel the animation immediately and revert to the
    /// pre-animation state (unless `fill: "forwards"` already
    /// committed).
    pub fn cancel(&self) {
        let _ = self.inner.cancel();
    }

    /// Fast-forward to the end; fires the `finish` event.
    pub fn finish(&self) {
        let _ = self.inner.finish();
    }

    /// Returns the underlying `web_sys::Animation` for escape-hatch
    /// use (pause, playbackRate, etc).
    pub fn raw(&self) -> &web_sys::Animation {
        &self.inner
    }

    /// Register a callback to fire when the animation finishes
    /// normally (not cancelled). Each call replaces the previous
    /// handler.
    pub fn on_finish<F: FnOnce() + 'static>(&self, cb: F) {
        let closure = Closure::once_into_js(cb);
        self.inner.set_onfinish(Some(closure.unchecked_ref()));
    }
}

/// Kick off a Web Animation on `el` with the given keyframes +
/// options. Returns a handle so callers can cancel or listen for
/// completion.
///
/// ```ignore
/// use pocopine::animate::{animate, AnimateOptions, Keyframe};
/// let handle = animate(
///     &el,
///     &[
///         Keyframe::from_iter([("opacity", "0"), ("transform", "scale(0.9)")]),
///         Keyframe::from_iter([("opacity", "1"), ("transform", "scale(1)")]),
///     ],
///     AnimateOptions { duration_ms: 180.0, ..Default::default() },
/// );
/// ```
pub fn animate(el: &Element, keyframes: &[Keyframe], opts: AnimateOptions) -> AnimationHandle {
    // Keyframes: `[{ property: value, … }, …]` as a JS array of
    // plain objects.
    let kf_array = Array::new();
    for kf in keyframes {
        let obj = Object::new();
        for (k, v) in &kf.props {
            let _ = Reflect::set(&obj, &JsValue::from_str(k), &JsValue::from_str(v));
        }
        kf_array.push(&obj);
    }

    // Options dict.
    let opt_obj = Object::new();
    let _ = Reflect::set(
        &opt_obj,
        &JsValue::from_str("duration"),
        &JsValue::from_f64(opts.duration_ms),
    );
    let _ = Reflect::set(
        &opt_obj,
        &JsValue::from_str("easing"),
        &JsValue::from_str(opts.easing),
    );
    if opts.delay_ms > 0.0 {
        let _ = Reflect::set(
            &opt_obj,
            &JsValue::from_str("delay"),
            &JsValue::from_f64(opts.delay_ms),
        );
    }
    let _ = Reflect::set(
        &opt_obj,
        &JsValue::from_str("fill"),
        &JsValue::from_str(opts.fill),
    );

    // Call `element.animate(keyframes, options)` via Reflect — we
    // don't want to depend on the full `web_sys::Animatable` trait
    // path (some browser versions ship slightly different shapes).
    let animate_fn = match Reflect::get(el.as_ref(), &JsValue::from_str("animate")) {
        Ok(v) if v.is_function() => v.unchecked_into::<js_sys::Function>(),
        _ => {
            // Element.animate unavailable — shouldn't happen on any
            // modern browser, but return a dummy inert animation
            // handle so callers don't crash.
            return AnimationHandle {
                inner: fallback_animation(),
            };
        }
    };

    let args = Array::new();
    args.push(&kf_array);
    args.push(&opt_obj);
    let result = animate_fn.apply(el.as_ref(), &args).unwrap_or(JsValue::NULL);
    let anim = result
        .dyn_into::<web_sys::Animation>()
        .unwrap_or_else(|_| fallback_animation());
    AnimationHandle { inner: anim }
}

/// Dummy Animation used when `element.animate` isn't available. It's
/// already finished and does nothing — keeps the return type
/// uniform so call sites don't need Option handling.
fn fallback_animation() -> web_sys::Animation {
    // Construct via JS: `new Animation()` is valid but the empty
    // ctor is non-standard; we use Reflect and fall back to a
    // plain Object cast.
    match js_sys::Reflect::construct(
        &js_sys::Function::new_no_args("return new Animation();"),
        &Array::new(),
    ) {
        Ok(v) => v.unchecked_into(),
        Err(_) => Object::new().unchecked_into(),
    }
}
