//! Keep's custom rich-text schema extensions.
//!
//! `KeepNoteSchemaExtension` shadows the default
//! `core_nodes` extension to add a `title` node type as the
//! mandatory first child of `doc`. Notes' title and body live in
//! one editor surface — the title is a structurally distinct
//! node so we can style it differently and extract it cleanly on
//! save without inferring intent from heading levels.
//!
//! Markdown emit: `title` serializes as a level-1 heading
//! (`# Title`). Apps importing a markdown note can promote the
//! first leading `#` to a title in a post-process pass; this
//! extension intentionally doesn't try to round-trip the
//! distinction (the surface stores titles as `title` nodes,
//! but markdown's vocabulary doesn't have a separate title
//! token — heading is the closest match).

use std::sync::Arc;

use pine_richtext::extension::RichTextExtension;
use pine_richtext::markdown::pulldown_cmark::{
    Event as MdEvent, HeadingLevel, Tag as MdTag, TagEnd as MdTagEnd,
};
use pine_richtext::markdown::NodeEmitter;
use pine_richtext::model::{MarkPolicy, NodeSpec, Whitespace};
use serde_json::json;

pub struct KeepNoteSchemaExtension;

impl RichTextExtension for KeepNoteSchemaExtension {
    fn name(&self) -> &str {
        // Same name as the default core-nodes extension so
        // `RuntimeBuilder`'s user-shadows-base overlay swaps us
        // in at the same fold position. Required: schema builder
        // rejects duplicate node names, so we have to REPLACE
        // the default `doc` spec rather than add a parallel one.
        "core_nodes"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            // doc: a `title` OR a `heading` may lead the document,
            // then any number of blocks. `title | heading` is a
            // necessary loosening for the markdown round-trip: the
            // serializer emits `# Title` for the title node, but
            // pulldown-cmark always parses `# Title` back as a
            // heading-level-1, not a title. Accepting either as the
            // leading slot lets `editor.set::<Markdown>(saved_body)`
            // succeed without a custom parse-rule. Save / load still
            // pulls the title out via the markdown string (the
            // `# ` prefix), so the data path is unchanged — only
            // the in-memory node-type for the first child differs
            // between freshly-typed (title) and reloaded (heading)
            // states. CSS targets both `data-type='title'` and a
            // first-child `data-type='heading'` to render the same
            // way.
            NodeSpec::new("doc").content("(title | heading) block*"),
            // title: own group (NOT "block") so it can't appear
            // mid-document — only at the top. Inline content,
            // defining boundary so split/join treat it like a
            // standalone textblock. No marks — titles stay
            // plain text so list cards can render them directly
            // without a markdown pass.
            NodeSpec::new("title")
                .group("title")
                .content("inline*")
                .marks(MarkPolicy::None)
                .defining(),
            // Rest of the default `core_nodes` set, verbatim.
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

    fn markdown_node_emitters(&self) -> Vec<(String, NodeEmitter)> {
        // Render `title` as `# Title` on export. Round-trip back
        // through the parser produces a regular heading-level-1;
        // callers that care about preserving the title role
        // should reload through the data model, not by parsing
        // their own markdown.
        vec![(
            "title".into(),
            Arc::new(|node, _parent, _index, sink| {
                let level = HeadingLevel::H1;
                sink.push(MdEvent::Start(MdTag::Heading {
                    level,
                    id: None,
                    classes: Vec::new(),
                    attrs: Vec::new(),
                }));
                for (i, child) in node.content().iter().enumerate() {
                    sink.render_node(child, node, i);
                }
                sink.push(MdEvent::End(MdTagEnd::Heading(level)));
            }),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::extensions::CoreMarksExtension;
    use pine_richtext::runtime::RuntimeBuilder;

    fn build_keep_runtime() -> std::sync::Arc<pine_richtext::runtime::EditorRuntime> {
        // Keep defaults on so `text`/inline come from `core_inline`,
        // marks come from `CoreMarksExtension`, etc. The keep schema
        // shadows `core_nodes` (same extension `name()`), which is
        // what lands the `title` node and the `(title | heading)
        // block*` content expression.
        RuntimeBuilder::new()
            .name("keep")
            .with(KeepNoteSchemaExtension)
            .with(CoreMarksExtension)
            .build()
    }

    #[test]
    fn default_node_doc_produces_schema_valid_empty_doc() {
        // `Editor::clear` uses `schema.default_node("doc")` to
        // synthesise an empty-but-valid doc — this is the
        // textarea-equivalent of `.value = ""`. Pin the result
        // here so a future schema tweak that drops the auto-
        // fill story doesn't silently break the `clear()`
        // path.
        let runtime = build_keep_runtime();
        let schema = runtime.schema().clone();
        let doc = schema
            .default_node("doc")
            .expect("keep schema must auto-fill `doc` content");
        assert_eq!(doc.type_name(), "doc");
        let first = doc
            .content()
            .iter()
            .next()
            .expect("default doc has at least one child to satisfy `(title | heading)`");
        assert!(
            matches!(first.type_name(), "title" | "heading"),
            "leading slot of default doc is title or heading, got `{}`",
            first.type_name()
        );
    }

    fn build_production_runtime() -> std::sync::Arc<pine_richtext::runtime::EditorRuntime> {
        // Mirror examples/keep/src/app.rs's production wiring so we
        // can exercise the exact parser the paste handler sees.
        RuntimeBuilder::new()
            .name("keep")
            .with(KeepNoteSchemaExtension)
            .with(pine_richtext::extensions::SmartTypographyExtension)
            .with(pine_richtext::extensions::MarkdownShortcutsExtension)
            .build()
    }

    #[test]
    fn user_pasted_markdown_parses_into_keep_schema() {
        // Regression for the in-browser report: pasting a chunk of
        // markdown with a level-2 heading and a bulleted list with
        // soft-wrapped lines must succeed against the production
        // runtime (KeepNoteSchemaExtension + Smart + Shortcuts, with
        // CoreMarks pulled in via the default-extensions overlay).
        let runtime = build_production_runtime();
        let parser = runtime.markdown_parser();
        let schema = runtime.schema().clone();
        let md = "## What I should *not* do\n\n\
                  - Don't start in TypeScript/Node — we have a Rust deploy crate\n  \
                  already; rewriting it in TS would be sad.\n\
                  - Don't try to support Kubernetes in v1 — Docker-only is the entire\n  \
                  scope of Option A.\n";
        let doc = parser.parse(md, &schema).expect("paste markdown parses");
        assert!(doc.child_count() >= 2, "doc has heading + list");
        let first = doc.child(0).unwrap();
        assert_eq!(first.type_name(), "heading");
        let second = doc.child(1).unwrap();
        assert!(
            matches!(second.type_name(), "bullet_list" | "list"),
            "second child should be a bullet list, got `{}`",
            second.type_name()
        );
    }

    #[test]
    fn markdown_with_leading_heading_round_trips_through_keep_schema() {
        // Regression for the "save then reopen loses content" bug:
        // the markdown serializer emits `# Title` for the `title`
        // node, but pulldown-cmark always parses `# Title` back as
        // a heading-1. With the original `title block*` content
        // expression this re-parse failed schema validation, so
        // `editor.set::<Markdown>(saved_body)` errored and the
        // surface stayed empty after a save → reopen. The
        // `(title | heading) block*` form accepts the heading-1 in
        // the title slot directly.
        let runtime = build_keep_runtime();
        let parser = runtime.markdown_parser();
        let schema = runtime.schema().clone();
        let md = "# My note title\n\nThe body of the note.\n";
        let doc = parser
            .parse(md, &schema)
            .expect("markdown with leading heading-1 should re-parse against the keep schema");
        let first = doc.content().iter().next().expect("doc has a first child");
        assert!(
            matches!(first.type_name(), "title" | "heading"),
            "leading slot must accept either title or heading; got `{}`",
            first.type_name()
        );
    }

    #[test]
    fn paste_multi_block_markdown_into_keep_body_replaces_selection() {
        // The default schema is `block+`, but keep is `(title |
        // heading) block*`. A paste containing a heading + list at a
        // body-paragraph cursor must satisfy that stricter content
        // expression — the unit tests in pine-richtext only cover
        // the default schema. Build a keep-shaped doc with cursor in
        // the body paragraph and dispatch the same slice the paste
        // handler would build.
        use pine_richtext::model::{Attrs, Fragment, Slice};
        use pine_richtext::state::{EditorState, EditorStateConfig, Selection};

        let runtime = build_production_runtime();
        let parser = runtime.markdown_parser();
        let schema = runtime.schema().clone();

        // Build a keep doc directly via the schema (round-tripping
        // through markdown would parse the title's `#` back as a
        // heading, not a title — fine for many tests, but we want
        // the actual `title` node here to confirm the body slot
        // accepts pasted blocks even when slot 1 is `title`).
        let title_text = schema.text("My note", Vec::new()).unwrap();
        let title = schema
            .node("title", Attrs::new(), Fragment::from(vec![title_text]))
            .unwrap();
        let body_text = schema.text("body text", Vec::new()).unwrap();
        let body = schema
            .node("paragraph", Attrs::new(), Fragment::from(vec![body_text]))
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(vec![title, body]))
            .unwrap();
        let cursor_in_body = doc.content_size() - 5;
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone(), doc).selection(Selection::text(cursor_in_body)),
        )
        .unwrap();

        let parsed = parser
            .parse(
                "## Pasted heading\n\n- item one\n- item two\n",
                state.schema(),
            )
            .expect("paste markdown parses");
        let content = parsed.content().clone();
        let mut tr = state.tr();
        tr.replace_selection(Slice::new(content, 0, 0))
            .expect("replace_selection accepts multi-block paste at body cursor in keep schema");
        let next = state.apply(tr).unwrap();
        let text = next.doc().text_content();
        assert!(text.contains("Pasted heading"), "heading present: {text:?}");
        assert!(text.contains("item one"), "list item present: {text:?}");
        let mut saw_heading = false;
        let mut saw_list = false;
        for i in 0..next.doc().child_count() {
            match next.doc().child(i).unwrap().type_name() {
                "heading" => saw_heading = true,
                "bullet_list" | "list" => saw_list = true,
                _ => {}
            }
        }
        assert!(saw_heading, "pasted heading survives at body slot");
        assert!(saw_list, "pasted bullet list survives at body slot");
    }

    #[test]
    fn empty_title_legacy_envelope_loads_without_silent_failure() {
        // Regression: pre-`body_state` notes flow through
        // `combine_title_and_body(title, body)` →
        // `editor.set::<Markdown>(...)`. For empty-title notes
        // the helper now prepends `# ` so the keep schema's
        // `(title | heading) block*` content expression
        // accepts the parse; previously body-only markdown
        // failed silently, leaving the surface empty and
        // letting the next save overwrite the note with
        // nothing.
        let runtime = build_keep_runtime();
        let parser = runtime.markdown_parser();
        let schema = runtime.schema().clone();
        let md = "# \n\nLegacy body only.\n";
        let doc = parser
            .parse(md, &schema)
            .expect("empty-title legacy markdown envelope parses");
        let first = doc.content().iter().next().expect("doc has a first child");
        assert!(
            matches!(first.type_name(), "title" | "heading"),
            "leading slot accepts an empty heading; got `{}`",
            first.type_name()
        );
        let body_present = (0..doc.child_count()).any(|i| {
            doc.child(i)
                .map(|c| c.text_content().contains("Legacy body only"))
                .unwrap_or(false)
        });
        assert!(body_present, "body text survives the parse");
    }

    #[test]
    fn body_without_leading_heading_still_round_trips() {
        // A note can have an empty title and just a body; the
        // surface emits `\nBody.` (no `# `) in that case, and
        // the parsed doc has no `title` first. Accepting a
        // leading block-group element (paragraph here) without
        // a title would be a regression — the schema still
        // requires a `title` or `heading` first. Confirm the
        // expected failure mode.
        let runtime = build_keep_runtime();
        let parser = runtime.markdown_parser();
        let schema = runtime.schema().clone();
        let md = "Body without title.\n";
        let result = parser.parse(md, &schema);
        assert!(
            result.is_err(),
            "doc without a title or leading heading should still reject — \
             the load path prepends `# ` for empty-title notes via `combine_title_and_body`"
        );
    }
}
