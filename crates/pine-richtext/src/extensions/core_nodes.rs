//! Block-level "core" nodes: `doc`, `paragraph`, `blockquote`,
//! `horizontal_rule`, `heading`, `code_block`. Mirrors the leading
//! `Schema::builder()` calls in pre-extension `schema_basic::schema()`.

use serde_json::json;

use crate::extension::RichTextExtension;
use crate::model::{MarkPolicy, NodeSpec, Whitespace};

pub struct CoreNodesExtension;

impl RichTextExtension for CoreNodesExtension {
    fn name(&self) -> &str {
        "core_nodes"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("doc").content("block+"),
            NodeSpec::new("paragraph").group("block").content("inline*"),
            NodeSpec::new("blockquote")
                .group("block")
                .content("block+")
                .defining(),
            NodeSpec::new("horizontal_rule").group("block").atom(),
            NodeSpec::new("heading")
                .group("block")
                .content("inline*")
                .defining()
                .attr("level", json!(1)),
            NodeSpec::new("code_block")
                .group("block")
                .content("text*")
                .marks(MarkPolicy::None)
                .code()
                .whitespace(Whitespace::Pre)
                .defining(),
        ]
    }
}
