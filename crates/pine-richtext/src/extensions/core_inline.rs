//! Inline / leaf nodes: `text`, `image`, `hard_break`. Mirrors the
//! tail of pre-extension `schema_basic::schema()`'s node list.

use serde_json::Value;

use crate::extension::RichTextExtension;
use crate::model::{MarkPolicy, NodeSpec};

pub struct CoreInlineExtension;

impl RichTextExtension for CoreInlineExtension {
    fn name(&self) -> &str {
        "core_inline"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("text").group("inline").inline(),
            NodeSpec::new("image")
                .group("inline")
                .inline()
                .atom()
                .marks(MarkPolicy::None)
                .required_attr("src")
                .attr("alt", Value::Null)
                .attr("title", Value::Null),
            NodeSpec::new("hard_break")
                .group("inline")
                .inline()
                .atom()
                .non_selectable(),
        ]
    }
}
