//! Wire types for the HN clone.
//!
//! Matches the shape of the Hacker News Firebase API items:
//! <https://github.com/HackerNews/API>.

use serde::{Deserialize, Serialize};

/// A single item (story, comment, job, …). `#[serde(default)]` on
/// every field so missing keys parse as their type's default — the HN
/// API omits fields liberally depending on item kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Item {
    pub id: u32,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub dead: bool,
    #[serde(default)]
    pub parent: u32,
    #[serde(default)]
    pub kids: Vec<u32>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub descendants: u32,
}
