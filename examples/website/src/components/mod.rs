//! Page components. Each subdirectory holds one `#[component]`
//! plus its sibling `.poco` template — the same layout the `hn`
//! example uses.
//!
//! The root `WebsiteApp` composes these by dropping the
//! components' custom tags into `WebsiteApp.poco`:
//!
//! ```html
//! <hero></hero>
//! <tutorial></tutorial>
//! <!-- …primitive showcase below… -->
//! ```

pub mod hero;
pub mod tutorial;

pub use hero::Hero;
pub use tutorial::Tutorial;
