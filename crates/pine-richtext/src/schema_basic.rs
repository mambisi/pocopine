use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

use crate::extension::{registry, RichTextExtension};
use crate::extensions::default_extensions;
use crate::model::{Attrs, Fragment, Mark, Node, Schema};
use crate::RichTextResult;

/// Return the basic rich text schema, composed by folding the canonical
/// set of base extensions (see [`crate::extensions::default_extensions`])
/// — possibly with same-named entries shadowed by user-registered
/// extensions ([`crate::extension::registry::register`]) — followed by
/// any extra user-registered extensions whose `name()` doesn't match a
/// base. The first call to this function seals the registry:
/// subsequent `register(…)` calls panic.
///
/// **User-name-wins semantics**: an app that calls
/// `extension::register(TaskListExtension::with_node_view::<C>())`
/// effectively replaces the default `TaskListExtension::new()` in the
/// fold position, preserving today's schema-rank order while letting
/// the user override node-view bindings, commands, or key bindings.
pub fn schema() -> Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            // Build the base extension chain as `Arc`s so they can be
            // shared between the schema fold and the user-overlay
            // resolution below. **Phase 4b C5:** the legacy
            // `install_base_extensions` is gone (the BASE table was
            // deleted). The schema fold is now purely structural; the
            // command/keymap/plugin tables live on the per-instance
            // `EditorRuntime` that the view actually reads from.
            let base: Vec<Arc<dyn RichTextExtension>> = default_extensions()
                .into_iter()
                .map(|boxed| -> Arc<dyn RichTextExtension> { Arc::from(boxed) })
                .collect();

            registry::mark_schema_realized();

            // Resolve the effective extension chain. For each base
            // slot, swap in the user-registered extension with the
            // same `name()` if present; otherwise keep the base. Then
            // append any user extensions with names that don't appear
            // in the base set.
            let user: Vec<Arc<dyn RichTextExtension>> = registry::registered();
            let base_names: HashSet<String> = base.iter().map(|e| e.name().to_string()).collect();

            let mut effective: Vec<Arc<dyn RichTextExtension>> = Vec::with_capacity(base.len());
            for base_ext in &base {
                let replacement = user.iter().find(|u| u.name() == base_ext.name());
                effective.push(replacement.cloned().unwrap_or_else(|| base_ext.clone()));
            }
            for user_ext in &user {
                if !base_names.contains(user_ext.name()) {
                    effective.push(user_ext.clone());
                }
            }

            let mut builder = Schema::builder();
            for ext in &effective {
                for spec in ext.nodes() {
                    builder = builder.node(spec);
                }
                for spec in ext.marks() {
                    builder = builder.mark(spec);
                }
            }
            builder.finish().expect("composed basic schema is valid")
        })
        .clone()
}

/// Alias for [`schema`].
pub fn basic_schema() -> Schema {
    schema()
}

/// Build a document node.
pub fn doc(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("doc", Attrs::new(), Fragment::from(children))
}

/// Build a paragraph node.
pub fn paragraph(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("paragraph", Attrs::new(), Fragment::from(children))
}

/// Build a blockquote node.
pub fn blockquote(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("blockquote", Attrs::new(), Fragment::from(children))
}

/// Build a horizontal rule node.
pub fn horizontal_rule() -> RichTextResult<Node> {
    schema().node("horizontal_rule", Attrs::new(), Fragment::empty())
}

/// Build a heading node with a numeric level attribute.
pub fn heading(level: u8, children: Vec<Node>) -> RichTextResult<Node> {
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(level.clamp(1, 6)));
    schema().node("heading", attrs, Fragment::from(children))
}

/// Build a code block. The text is stored as plain `text` children.
pub fn code_block(text: impl Into<String>) -> RichTextResult<Node> {
    let text = text.into();
    let content = if text.is_empty() {
        Fragment::empty()
    } else {
        Fragment::from(schema().text(text, Vec::new())?)
    };
    schema().node("code_block", Attrs::new(), content)
}

/// Build a text node.
pub fn text(value: impl Into<String>, marks: Vec<Mark>) -> RichTextResult<Node> {
    schema().text(value, marks)
}

/// Build an image node.
pub fn image(
    src: impl Into<String>,
    alt: Option<impl Into<String>>,
    title: Option<impl Into<String>>,
) -> RichTextResult<Node> {
    let mut attrs = Attrs::new();
    attrs.insert("src".to_string(), Value::String(src.into()));
    if let Some(alt) = alt {
        attrs.insert("alt".to_string(), Value::String(alt.into()));
    }
    if let Some(title) = title {
        attrs.insert("title".to_string(), Value::String(title.into()));
    }
    schema().node("image", attrs, Fragment::empty())
}

/// Build a hard break node.
pub fn hard_break() -> RichTextResult<Node> {
    schema().node("hard_break", Attrs::new(), Fragment::empty())
}

/// Build a list item node.
pub fn list_item(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("list_item", Attrs::new(), Fragment::from(children))
}

/// Build a bullet list node.
pub fn bullet_list(items: Vec<Node>) -> RichTextResult<Node> {
    schema().node("bullet_list", Attrs::new(), Fragment::from(items))
}

/// Build an ordered list node.
pub fn ordered_list(items: Vec<Node>) -> RichTextResult<Node> {
    schema().node("ordered_list", Attrs::new(), Fragment::from(items))
}

/// Build a task (checklist) item node.
pub fn task_item(checked: bool, children: Vec<Node>) -> RichTextResult<Node> {
    let mut attrs = Attrs::new();
    attrs.insert("checked".to_string(), json!(checked));
    schema().node("task_item", attrs, Fragment::from(children))
}

/// Build a task (checklist) list node.
pub fn task_list(items: Vec<Node>) -> RichTextResult<Node> {
    schema().node("task_list", Attrs::new(), Fragment::from(items))
}

/// Build a link mark.
pub fn link(href: impl Into<String>, title: Option<impl Into<String>>) -> RichTextResult<Mark> {
    let mut attrs = Attrs::new();
    attrs.insert("href".to_string(), Value::String(href.into()));
    if let Some(title) = title {
        attrs.insert("title".to_string(), Value::String(title.into()));
    }
    schema().mark("link", attrs)
}

/// Build an emphasis mark.
pub fn em() -> RichTextResult<Mark> {
    schema().mark("em", Attrs::new())
}

/// Build a strong mark.
pub fn strong() -> RichTextResult<Mark> {
    schema().mark("strong", Attrs::new())
}

/// Build a code mark.
pub fn code() -> RichTextResult<Mark> {
    schema().mark("code", Attrs::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_builders_create_valid_document() {
        let strong = strong().unwrap();
        let document = doc(vec![
            heading(2, vec![text("Title", Vec::new()).unwrap()]).unwrap(),
            paragraph(vec![
                text("Hello ", Vec::new()).unwrap(),
                text("world", vec![strong]).unwrap(),
                hard_break().unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();

        schema().check_node(&document).unwrap();
        assert_eq!(document.text_content(), "TitleHello world");
    }

    #[test]
    fn basic_schema_rejects_blocks_inside_paragraph() {
        let nested = blockquote(vec![paragraph(Vec::new()).unwrap()]).unwrap();
        let err = paragraph(vec![nested]).unwrap_err();
        assert!(err.to_string().contains("paragraph cannot contain"));
    }

    #[test]
    fn leaf_and_empty_container_sizes_are_distinct() {
        let paragraph = paragraph(Vec::new()).unwrap();
        let hard_break = hard_break().unwrap();

        assert!(!paragraph.is_leaf());
        assert!(hard_break.is_leaf());
        assert_eq!(paragraph.node_size(), 2);
        assert_eq!(hard_break.node_size(), 1);

        let decoded: Node =
            serde_json::from_value(serde_json::to_value(&hard_break).unwrap()).unwrap();
        assert_eq!(decoded, hard_break);
        schema().check_node(&decoded).unwrap();
    }
}
