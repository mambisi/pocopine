//! Types shared between the client and server halves of the blog example.
//!
//! Every type on the wire is `Serialize + Deserialize`. Keep this module
//! small — the larger it grows, the more of your domain becomes
//! client-visible.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Post {
    pub id: u32,
    pub title: String,
    pub body: String,
}
