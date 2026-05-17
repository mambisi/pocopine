//! Markdown shortcuts extension — typing `# ` makes a heading,
//! `* ` makes a bullet list, `1. ` makes an ordered list, `> ` makes
//! a blockquote, ``` ``` ``` makes a code block.
//!
//! Each shortcut is an input rule that fires at the start of a
//! textblock when its marker is typed. The rule deletes the marker
//! and either changes the textblock type (heading, code block) or
//! wraps the textblock in a new node (blockquote, lists).
//!
//! The schema must declare the target node types — `heading`,
//! `code_block`, `bullet_list`, `ordered_list`, `blockquote`. The
//! built-in default extensions provide all of these; an app with a
//! custom schema (e.g. the demo's `CommentSchemaExtension`) that
//! omits some of them gets a silent no-op for the missing rules
//! (the schema lookup fails inside the handler and `None` falls
//! through to the default insert).

use regex::Regex;
use serde_json::json;

use crate::extension::RichTextExtension;
use crate::inputrules::{textblock_type_input_rule, wrapping_input_rule, InputRule};
use crate::model::Attrs;

/// Markdown-style block-shortcut input rules.
///
/// * `# ` → H1, `## ` → H2, …, `###### ` → H6
/// * ``` ``` ``` → code block
/// * `> ` → blockquote
/// * `* ` / `- ` / `+ ` → bullet list
/// * `1. ` (any number) → ordered list with `order` attr
pub struct MarkdownShortcutsExtension;

impl RichTextExtension for MarkdownShortcutsExtension {
    fn name(&self) -> &str {
        "markdown-shortcuts"
    }

    fn input_rules(&self) -> Vec<InputRule> {
        vec![
            // Headings — `^ #{1,6}\s$` captures level 1-6. The match's
            // group-1 is the `#` run; its length is the level.
            textblock_type_input_rule(
                Regex::new(r"^(#{1,6})\s$").unwrap(),
                "heading",
                |captures| {
                    let level = captures
                        .get(1)
                        .map(|m| m.as_str().len().clamp(1, 6))
                        .unwrap_or(1);
                    let mut attrs = Attrs::new();
                    attrs.insert("level".into(), json!(level as u8));
                    attrs
                },
            ),
            // Code block — ``` ``` ``` at start of a textblock.
            textblock_type_input_rule(Regex::new(r"^```$").unwrap(), "code_block", |_| {
                Attrs::new()
            }),
            // Blockquote — `> `.
            wrapping_input_rule(
                Regex::new(r"^\s*>\s$").unwrap(),
                "blockquote",
                |_| Attrs::new(),
                None,
            ),
            // Bullet list — `* `, `- `, `+ `.
            wrapping_input_rule(
                Regex::new(r"^\s*([-+*])\s$").unwrap(),
                "bullet_list",
                |_| Attrs::new(),
                None,
            ),
            // Ordered list — `1. ` (any non-zero integer).
            wrapping_input_rule(
                Regex::new(r"^\s*(\d+)\.\s$").unwrap(),
                "ordered_list",
                |captures| {
                    let order = captures
                        .get(1)
                        .and_then(|m| m.as_str().parse::<i64>().ok())
                        .unwrap_or(1);
                    let mut attrs = Attrs::new();
                    attrs.insert("order".into(), json!(order));
                    attrs
                },
                // Only auto-join with a preceding list when the new
                // list would START at the order right after the
                // prior list's last item — otherwise a `5. ` at the
                // bottom of a 1-2 list would NOT merge, which
                // mirrors what most markdown editors do.
                Some(Box::new(
                    |captures: &regex::Captures<'_>, before: &crate::model::Node| {
                        let order = captures
                            .get(1)
                            .and_then(|m| m.as_str().parse::<i64>().ok())
                            .unwrap_or(1);
                        let before_order = before
                            .attrs()
                            .get("order")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(1);
                        before_order + before.child_count() as i64 == order
                    },
                )),
            ),
        ]
    }
}
