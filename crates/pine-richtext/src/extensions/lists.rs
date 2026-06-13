//! Lists schema (`bullet_list`, `ordered_list`, `list_item`) + commands
//! that wrap content into them.
//!
//! Three node types are grouped under one extension because they form
//! a single schema unit — bullet/ordered lists both require list items.
//! Splitting them would break today's `schema_basic::schema()` node
//! ordering (`bullet_list, ordered_list, list_item`) which the snapshot
//! test enforces in C2.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::commands::{BoxedCommand, wrap_in_list};
use crate::extension::{NamedCommand, RichTextExtension};
use crate::model::{Attrs, NodeSpec};

pub struct ListsExtension;

impl ListsExtension {
    /// Build the `wrap_in_<list_type>` named command exposed to the
    /// `pine:richtext:command` `Custom { name, args }` variant.
    /// `args` is an optional object `{ "attrs": { … } }` that defaults
    /// to empty when omitted or malformed.
    fn wrap_command(list_type: &'static str, item_type: &'static str) -> NamedCommand {
        Arc::new(move |args: Value| -> Option<BoxedCommand> {
            let attrs = args
                .get("attrs")
                .cloned()
                .and_then(|v| serde_json::from_value::<Attrs>(v).ok())
                .unwrap_or_default();
            Some(wrap_in_list(list_type, item_type, attrs))
        })
    }
}

impl RichTextExtension for ListsExtension {
    fn name(&self) -> &str {
        "lists"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("bullet_list")
                .group("block")
                .content("list_item+"),
            NodeSpec::new("ordered_list")
                .group("block")
                .content("list_item+")
                .attr("order", json!(1)),
            NodeSpec::new("list_item")
                .content("paragraph block*")
                .defining(),
        ]
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        vec![
            (
                "wrap_in_bullet_list".into(),
                Self::wrap_command("bullet_list", "list_item"),
            ),
            (
                "wrap_in_ordered_list".into(),
                Self::wrap_command("ordered_list", "list_item"),
            ),
        ]
    }

    fn list_item_types(&self) -> &'static [&'static str] {
        &["list_item"]
    }
}
