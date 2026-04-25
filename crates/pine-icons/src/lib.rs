//! Tabler Icons (MIT) packaged for pocopine / Pine apps.
//!
//! Two call sites, one sanctioned path each:
//!
//! **Rust handlers** — compile-time literal, tree-shaken per call:
//!
//! ```ignore
//! use pine_icons::icon;
//!
//! let user_svg   = icon!("user");                // outline (default)
//! let star_svg   = icon!(filled / "star");
//! ```
//!
//! **Templates** — declare-then-use. List icons your app needs in
//! a `register_icons!` call at startup so the `<pine-icon>`
//! primitive can look them up by name. Only registered icons ship
//! in the WASM binary.
//!
//! ```ignore
//! #[wasm_bindgen(start)]
//! fn main() {
//!     pine_icons::register_icons!["user", "chevron-down", filled / "star"];
//!     App::new().register::<pine_icons::PineIcon>().run();
//! }
//! ```
//!
//! ```html
//! <pine-icon name="user"></pine-icon>
//! <pine-icon name="star" variant="filled" size="16"></pine-icon>
//! <pine-icon :name="current_icon"></pine-icon>
//! ```

use std::cell::RefCell;

pub use pine_icons_macros::{icon, register_icons};

mod primitive;
pub use primitive::PineIcon;

/// One entry in the runtime registry. Construction is restricted
/// to code emitted by [`register_icons!`] — authors never write
/// this directly, so the fields can stay public without risking
/// misuse.
///
/// `svg_hash` is a compile-time FNV-1a 64-bit hash of the SVG
/// bytes. It lets change-detection on a re-rendered `<pine-icon>`
/// be O(1) instead of an O(n) byte-by-byte compare — meaningful
/// when the registered set includes illustration-sized SVGs.
pub struct IconEntry {
    pub name: &'static str,
    pub variant: &'static str,
    pub svg: &'static str,
    pub svg_hash: u64,
}

thread_local! {
    // Vec, not HashMap — the registered set is small (tens, not
    // thousands), and a linear scan beats the per-lookup hash cost
    // at that scale. All entries live for the app's lifetime, so
    // no eviction or mutation-after-first-lookup semantics needed.
    static REGISTRY: RefCell<Vec<IconEntry>> = const { RefCell::new(Vec::new()) };
}

/// Register a pre-loaded icon entry at app startup. Called by
/// [`register_icons!`] — not intended for direct use.
#[doc(hidden)]
pub fn __register(entry: IconEntry) {
    REGISTRY.with(|r| r.borrow_mut().push(entry));
}

/// Return the SVG for a registered icon, or `None` if the icon
/// wasn't included via [`register_icons!`].
///
/// `variant` is `"outline"` (default) or `"filled"`.
pub fn lookup(variant: &str, name: &str) -> Option<&'static str> {
    lookup_with_hash(variant, name).map(|(svg, _)| svg)
}

/// Sentinel tuple for an unresolved icon. Callers that prefer a
/// branch-free miss path can write
/// `lookup_with_hash(v, n).unwrap_or(EMPTY_ICON)`.
///
/// The hash sentinel is `0`, which no real FNV-1a value can
/// collide with: the algorithm seeds with a nonzero offset and
/// multiplies by a nonzero prime, so every non-empty input
/// produces a nonzero hash.
pub const EMPTY_ICON: (&str, u64) = ("", 0u64);

/// Same as [`lookup`] but also returns the precomputed FNV-1a
/// hash of the SVG bytes. Useful for O(1) change detection when
/// a primitive needs to decide whether to replay an effect on a
/// re-render — avoids a full string compare on large icons.
pub fn lookup_with_hash(variant: &str, name: &str) -> Option<(&'static str, u64)> {
    REGISTRY.with(|r| {
        r.borrow()
            .iter()
            .find(|e| e.variant == variant && e.name == name)
            .map(|e| (e.svg, e.svg_hash))
    })
}

/// Number of icons currently registered — useful for tests and
/// the dev-mode devtools panel.
pub fn registry_len() -> usize {
    REGISTRY.with(|r| r.borrow().len())
}
