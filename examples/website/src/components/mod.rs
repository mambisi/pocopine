//! Page components. Each subdirectory holds one `#[component]`
//! plus its sibling `.poco` template — the same layout the `hn`
//! example uses.

pub mod hero;
pub mod showcase;
pub mod showcase_card;
pub mod tutorial;

pub use hero::Hero;
pub use showcase::Showcase;
pub use showcase_card::ShowcaseCard;
pub use tutorial::Tutorial;
