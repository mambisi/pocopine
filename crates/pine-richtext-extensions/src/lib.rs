//! Optional extensions for `pine-richtext`.
//!
//! Model features such as `tables` stay target-independent. Browser UI is
//! separately gated, so applications that only need the schema and transaction
//! layer do not pull in Pocopine or the DOM.

#[cfg(feature = "bubble-menu")]
pub mod bubble_menu;

#[cfg(feature = "tables")]
pub mod tables;

#[cfg(feature = "tags")]
pub mod tags;
