#![cfg(feature = "tags")]

use std::sync::Arc;

use pine_richtext::model::{Attrs, Fragment, Node};
use pine_richtext::render::render_doc_to_html;
use pine_richtext::runtime::{EditorRuntime, RuntimeBuilder};
use pine_richtext::state::{EditorState, EditorStateConfig, Selection};
use pine_richtext_extensions::tags::{
    TagAttrs, TagClipboardPayload, TagKind, TagNode, TagSuggestionConfig, TagSuggestionMatcher,
    TagsExtension, delete_tag_at, insert_tag, leave_selected_tag_left, leave_selected_tag_right,
    select_tag_at, select_tag_backward, select_tag_forward, update_tag_at,
};
use serde_json::Value;

fn runtime() -> Arc<EditorRuntime> {
    RuntimeBuilder::new().with(TagsExtension).build()
}

fn attrs(value: &TagAttrs) -> Attrs {
    match serde_json::to_value(value).unwrap() {
        Value::Object(object) => object.into_iter().collect(),
        _ => unreachable!("tag attrs serialize as an object"),
    }
}

fn paragraph_doc(runtime: &EditorRuntime, children: Vec<Node>) -> Node {
    let paragraph = runtime
        .schema()
        .node("paragraph", Attrs::new(), Fragment::from(children))
        .unwrap();
    runtime
        .schema()
        .node("doc", Attrs::new(), Fragment::from(paragraph))
        .unwrap()
}

fn empty_state(runtime: &Arc<EditorRuntime>) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(runtime.schema().clone(), paragraph_doc(runtime, Vec::new()))
            .selection(Selection::text(1)),
    )
    .unwrap()
}

#[test]
fn typed_schema_materializes_closed_versioned_inline_atoms() {
    let runtime = runtime();
    let typed = runtime.typed_node::<TagNode>().expect("typed tag spec");
    assert_eq!(typed.name(), "tag");
    assert!(typed.spec().is_inline());
    assert!(typed.spec().is_atom());

    let attrs = TagAttrs::new("priority", "Priority")
        .unwrap()
        .with_kind(TagKind::Warning);
    let tag = runtime
        .schema()
        .node("tag", attrs.to_attrs().unwrap(), Fragment::empty())
        .unwrap();
    assert_eq!(tag.version(), Some(1));
    assert_eq!(
        runtime.schema().leaf_text_for(&tag).as_deref(),
        Some("#Priority")
    );

    let encoded = serde_json::to_value(&tag).unwrap();
    assert_eq!(encoded["type"], "tag");
    assert_eq!(encoded["version"], 1);
    assert_eq!(encoded["attrs"]["id"], "priority");
    assert_eq!(encoded["attrs"]["kind"], "warning");
}

#[test]
fn native_dom_fallback_is_semantic_accessible_and_deterministic() {
    let runtime = runtime();
    let tag_attrs = TagAttrs::new("priority", "Priority")
        .unwrap()
        .with_kind(TagKind::Warning);
    let tag = runtime
        .schema()
        .node("tag", attrs(&tag_attrs), Fragment::empty())
        .unwrap();
    let html = render_doc_to_html(&runtime, &paragraph_doc(&runtime, vec![tag]));

    assert!(runtime.lookup_dom_view("tag").is_some());
    assert!(html.contains(r#"class="pine-richtext-tag pine-richtext-tag--native""#));
    assert!(html.contains(r#"data-pine-tag="true""#));
    assert!(html.contains(r#"data-tag-id="priority""#));
    assert!(html.contains(r#"data-kind="warning""#));
    assert!(html.contains(r#"contenteditable="false""#));
    assert!(html.contains(r#"role="group""#));
    assert!(html.contains(r#"aria-roledescription="tag""#));
    assert!(html.contains(r#"aria-label="Priority""#));
    assert!(
        html.contains(r#"<span aria-hidden="true" class="pine-richtext-tag__prefix">#</span>"#)
    );
    assert!(
        html.contains(
            r#"<span aria-hidden="true" class="pine-richtext-tag__label">Priority</span>"#
        )
    );
    assert!(!html.contains("pine-richtext-tag__remove"));
    assert!(!html.contains("PineRichTextTag"));
}

#[test]
fn native_dom_fallback_escapes_text_and_attribute_boundaries() {
    let runtime = runtime();
    let hostile = TagAttrs::new(
        r#"x\"><svg/onload=alert(1)>"#,
        r#"<img src=x onerror=alert(1)> & \"quoted\""#,
    )
    .unwrap()
    .with_kind(TagKind::Danger);
    let tag = runtime
        .schema()
        .node("tag", attrs(&hostile), Fragment::empty())
        .unwrap();
    let html = render_doc_to_html(&runtime, &paragraph_doc(&runtime, vec![tag]));

    assert!(!html.contains(r#"data-tag-id="x\"><"#));
    // html5ever is allowed to keep `<` literal inside a quoted attribute; it
    // cannot open a tag there. The security boundary is the hostile quote,
    // which must be escaped so the fixed `data-tag-id` value never ends.
    assert!(html.contains(r#"data-tag-id="x\&quot;><svg/onload=alert(1)>""#));
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(html.contains("&amp;"));
    assert!(html.contains("&quot;"));
    assert_eq!(html.matches("data-tag-id=").count(), 1);
}

#[test]
fn closed_tag_attrs_reject_dom_token_injection_before_rendering() {
    let runtime = runtime();
    let mut injected = Attrs::new();
    injected.insert("id".into(), Value::String("safe".into()));
    injected.insert("label".into(), Value::String("Safe".into()));
    injected.insert(
        "kind".into(),
        Value::String(r#"danger\" onclick=\"alert(1)"#.into()),
    );

    assert!(
        runtime
            .schema()
            .node("tag", injected, Fragment::empty())
            .is_err()
    );
}

#[test]
fn insert_update_select_and_delete_are_normal_transactions() {
    let runtime = runtime();
    let initial = TagAttrs::new("one", "One").unwrap();
    let state = empty_state(&runtime);

    let state = state
        .apply(insert_tag(initial).apply(&state).unwrap())
        .unwrap();
    let tag = state.doc().node_at(1).unwrap().unwrap();
    assert_eq!(tag.type_name(), "tag");
    assert_eq!(tag.attrs()["label"], "One");

    let selected = state
        .apply(select_tag_at(1).apply(&state).unwrap())
        .unwrap();
    assert_eq!(selected.selection(), &Selection::node(1));

    let replacement = TagAttrs::new("two", "Two")
        .unwrap()
        .with_kind(TagKind::Info);
    let updated = selected
        .apply(update_tag_at(1, replacement).apply(&selected).unwrap())
        .unwrap();
    assert_eq!(
        updated.doc().node_at(1).unwrap().unwrap().attrs()["id"],
        "two"
    );
    assert_eq!(
        updated.doc().node_at(1).unwrap().unwrap().attrs()["kind"],
        "info"
    );

    let deleted = updated
        .apply(delete_tag_at(1).apply(&updated).unwrap())
        .unwrap();
    assert!(deleted.doc().node_at(1).unwrap().is_none());
    assert_eq!(deleted.doc().child(0).unwrap().type_name(), "paragraph");
}

#[test]
fn base_keyboard_delete_treats_tag_as_one_inline_unit() {
    let runtime = runtime();
    let tag_attrs = TagAttrs::new("one", "One").unwrap();
    let tag = runtime
        .schema()
        .node("tag", attrs(&tag_attrs), Fragment::empty())
        .unwrap();
    let doc = paragraph_doc(&runtime, vec![tag]);
    let state = EditorState::create(
        EditorStateConfig::new(runtime.schema().clone(), doc).selection(Selection::text(2)),
    )
    .unwrap();

    let transaction = pine_richtext::commands::delete_node_backward()
        .apply(&state)
        .expect("backspace after tag deletes it");
    let next = state.apply(transaction).unwrap();
    assert_eq!(next.doc().child(0).unwrap().child_count(), 0);
    assert_eq!(next.selection(), &Selection::text(1));
}

#[test]
fn arrow_navigation_selects_then_moves_across_the_atom() {
    let runtime = runtime();
    let tag_attrs = TagAttrs::new("one", "One").unwrap();
    let tag = runtime
        .schema()
        .node("tag", attrs(&tag_attrs), Fragment::empty())
        .unwrap();
    let doc = paragraph_doc(&runtime, vec![tag]);

    let before = EditorState::create(
        EditorStateConfig::new(runtime.schema().clone(), doc.clone()).selection(Selection::text(1)),
    )
    .unwrap();
    let selected = before
        .apply(select_tag_forward().apply(&before).unwrap())
        .unwrap();
    assert_eq!(selected.selection(), &Selection::node(1));
    let after = selected
        .apply(leave_selected_tag_right().apply(&selected).unwrap())
        .unwrap();
    assert_eq!(after.selection(), &Selection::text(2));

    let selected = after
        .apply(select_tag_backward().apply(&after).unwrap())
        .unwrap();
    assert_eq!(selected.selection(), &Selection::node(1));
    let before_again = selected
        .apply(leave_selected_tag_left().apply(&selected).unwrap())
        .unwrap();
    assert_eq!(before_again.selection(), &Selection::text(1));
}

#[test]
fn markdown_and_clipboard_fallbacks_are_deterministic_for_all_kinds() {
    let runtime = runtime();
    let kinds = [
        None,
        Some(TagKind::Neutral),
        Some(TagKind::Info),
        Some(TagKind::Success),
        Some(TagKind::Warning),
        Some(TagKind::Danger),
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let mut tag_attrs = TagAttrs::new(format!("tag-{index}"), format!("Label{index}")).unwrap();
        tag_attrs.kind = kind;
        let tag = runtime
            .schema()
            .node("tag", attrs(&tag_attrs), Fragment::empty())
            .unwrap();
        let doc = paragraph_doc(&runtime, vec![tag]);
        assert_eq!(
            runtime.markdown_serializer().serialize(&doc).unwrap(),
            format!("\\#Label{index}")
        );

        let payload = TagClipboardPayload::new(tag_attrs);
        let encoded = payload.to_json().unwrap();
        assert_eq!(TagClipboardPayload::from_json(&encoded).unwrap(), payload);
        assert_eq!(payload.plain_text(), format!("#Label{index}"));
        assert_eq!(payload.markdown(), format!("\\#Label{index}"));
    }
}

#[test]
fn suggestion_ranges_are_bounded_and_never_underflow() {
    let matcher = TagSuggestionMatcher::new(TagSuggestionConfig {
        minimum_query_chars: 1,
        maximum_query_chars: 4,
        ..TagSuggestionConfig::default()
    });
    assert!(matcher.match_prefix("#", 1).is_none());
    assert!(matcher.match_prefix("#abcde", 6).is_none());
    assert!(matcher.match_prefix("#rust", 3).is_none());
    assert_eq!(
        matcher.match_prefix("hello #rust", 11).unwrap(),
        pine_richtext_extensions::tags::TagSuggestionMatch {
            trigger: '#',
            query: "rust".to_string(),
            from: 6,
            to: 11,
        }
    );
}

#[cfg(feature = "view")]
#[test]
fn typed_view_descriptor_proves_the_semantic_component_pair() {
    use pine_richtext::view::NodeViewSpec;
    use pine_richtext_extensions::tags::PineRichTextTag;
    use pocopine::Component;

    let spec = NodeViewSpec::atom_component::<TagNode, PineRichTextTag>();
    assert_eq!(spec.component_name(), PineRichTextTag::NAME);
    assert_eq!(spec.node_type(), "tag");
    assert_eq!(spec.semantic_type_id(), std::any::TypeId::of::<TagNode>());
}

#[cfg(feature = "view")]
#[test]
fn matcher_consumes_lightweight_change_and_selection_snapshots() {
    use pine_richtext::view::{ChangeInfo, SelectionSnapshot, ViewportRect};

    let change = ChangeInfo {
        generation: 9,
        empty: false,
        caret_prefix: "Assign #rust".to_string(),
    };
    let snapshot = SelectionSnapshot {
        selection: Selection::text(12),
        from: 12,
        to: 12,
        empty: true,
        active_mark_names: Vec::new(),
        enclosing_block_types: vec!["paragraph".to_string()],
        rect: Some(ViewportRect::default()),
        focused: true,
        editable: true,
    };
    let matched = TagSuggestionMatcher::default()
        .match_change(&change, &snapshot)
        .unwrap();
    assert_eq!(matched.query, "rust");
    assert_eq!((matched.from, matched.to), (7, 12));
}
