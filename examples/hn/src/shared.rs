//! Wire types shaped around the HN Algolia API:
//!   * `GET /api/v1/search?tags=front_page` → list of stories in one shot
//!   * `GET /api/v1/items/:id` → story + nested comment tree in one shot

use serde::{Deserialize, Serialize};

/// A search hit from `/api/v1/search`. The Algolia response is richer
/// than the Firebase one — we pull the fields we render and leave the
/// rest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Story {
    #[serde(rename = "objectID", default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub points: Option<i32>,
    #[serde(default)]
    pub num_comments: Option<u32>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at_i: i64,
    #[serde(default)]
    pub story_text: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub hits: Vec<Story>,
}

/// Recursive shape from `/api/v1/items/:id` — works for stories,
/// comments, polls. `children` is the (pre-populated) reply tree.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemNode {
    #[serde(default)]
    pub id: u64,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub points: Option<i32>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub created_at_i: i64,
    #[serde(default)]
    pub children: Vec<ItemNode>,
}
