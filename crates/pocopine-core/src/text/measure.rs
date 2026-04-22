//! Width measurement abstraction.
//!
//! [`Measurer`] decouples the layout engine from any specific font
//! system so the pure line-break / layout logic stays testable on
//! the host without a Canvas. [`CanvasMeasurer`] is the default
//! in-browser impl and forwards to the JS shim in [`super::measure_js`].

use crate::text::prepare::Font;

pub trait Measurer {
    /// Width of `text` rendered in `font`, in CSS pixels.
    fn measure(&self, font: &Font, text: &str) -> f64;
}

pub struct CanvasMeasurer;

impl Measurer for CanvasMeasurer {
    fn measure(&self, font: &Font, text: &str) -> f64 {
        super::measure_js::measure_text(&font.0, text)
    }
}
