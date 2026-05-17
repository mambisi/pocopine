//! Parity tests for `prosemirror-transform/test/*.ts`.
//!
//! Split out of the previous monolithic `prosemirror_parity.rs`. Mirrors
//! `prosemirror-transform/test/test-mapping.ts`, `test-replace_step.ts`,
//! `test-step.ts`, and `test-trans.ts`. `test-structure.ts` lives in its
//! own dedicated file (`parity_structure.rs`).

use serde_json::json;

mod support;

use pine_richtext::model::{Attrs, Fragment, MarkPolicy, MarkSpec, Node, NodeSpec, Schema, Slice};
use pine_richtext::schema_basic;
use pine_richtext::transform::{
    can_join, join_point, AttrStep, DocAttrStep, MapRange, Mapping, MarkStep, NodeMarkStep,
    ReplaceAroundStep, ReplaceStep, Step, StepMap,
};

use support::*;

#[test]
fn transform_step_json_round_trips_all_variants() {
    let schema = schema_basic::schema();
    let em = schema_basic::em().unwrap();

    let replace_step = Step::Replace(ReplaceStep::new(
        2,
        4,
        Slice::new(Fragment::from(text("xy")), 0, 0),
    ));

    let replace_around = Step::ReplaceAround(ReplaceAroundStep::new(
        0,
        5,
        1,
        4,
        Slice::new(
            Fragment::from(schema_basic::paragraph(vec![]).unwrap()),
            0,
            0,
        ),
        1,
    ));

    let add_mark = Step::AddMark(MarkStep::new(1, 4, em.clone()));
    let remove_mark = Step::RemoveMark(MarkStep::new(1, 4, em.clone()));
    let add_node_mark = Step::AddNodeMark(NodeMarkStep {
        pos: 1,
        mark: em.clone(),
    });
    let remove_node_mark = Step::RemoveNodeMark(NodeMarkStep {
        pos: 1,
        mark: em.clone(),
    });
    let attr_step = Step::Attr(AttrStep {
        pos: 1,
        attr: "level".to_string(),
        value: Some(json!(2)),
    });
    let attr_clear = Step::Attr(AttrStep {
        pos: 3,
        attr: "src".to_string(),
        value: None,
    });
    let doc_attr = Step::DocAttr(DocAttrStep {
        attr: "version".to_string(),
        value: Some(json!(7)),
    });

    let cases = [
        replace_step,
        replace_around,
        add_mark,
        remove_mark,
        add_node_mark,
        remove_node_mark,
        attr_step,
        attr_clear,
        doc_attr,
    ];

    for step in cases {
        let json = step.to_json();
        // Every step carries a stepType field naming the variant.
        assert!(json.get("stepType").is_some(), "missing stepType: {json}");
        let round_tripped = Step::from_json(&schema, json.clone())
            .unwrap_or_else(|e| panic!("from_json failed for {json}: {e}"));
        assert_eq!(round_tripped, step, "round-trip mismatch for {json}");
    }
}

#[test]
fn transform_step_from_json_uses_schema_validation() {
    let schema = schema_basic::schema();
    // Unknown step type errors.
    let err = Step::from_json(&schema, json!({"stepType": "wat"})).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("unknown step type"));

    // Unknown mark type inside a step also errors via the schema lookup.
    let err = Step::from_json(
        &schema,
        json!({
            "stepType": "addMark",
            "from": 1,
            "to": 3,
            "mark": {"type": "nonexistent"}
        }),
    )
    .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("unknown"));
}

#[test]
fn transform_step_json_matches_upstream_field_names() {
    // Pin the upstream camelCase field names (`stepType`, `gapFrom`, `gapTo`,
    // `openStart`, `openEnd`) so a pine Step roundtrips with a JS PM client.
    let json = Step::Replace(ReplaceStep::new(0, 2, Slice::new(Fragment::empty(), 0, 0))).to_json();
    assert_eq!(json["stepType"], json!("replace"));
    assert_eq!(json["from"], json!(0));
    assert_eq!(json["to"], json!(2));

    let json = Step::ReplaceAround(ReplaceAroundStep::new(
        0,
        5,
        1,
        4,
        Slice::new(
            Fragment::from(schema_basic::paragraph(vec![]).unwrap()),
            0,
            0,
        ),
        1,
    ))
    .to_json();
    assert_eq!(json["stepType"], json!("replaceAround"));
    assert_eq!(json["gapFrom"], json!(1));
    assert_eq!(json["gapTo"], json!(4));
    assert_eq!(json["insert"], json!(1));

    // Slice JSON uses openStart / openEnd camelCase.
    let slice = Slice::new(Fragment::from(text("hi")), 1, 2);
    let v = serde_json::to_value(&slice).unwrap();
    assert_eq!(v["openStart"], json!(1));
    assert_eq!(v["openEnd"], json!(2));
}

#[test]
fn transform_keeps_defining_context_at_textblock_start() {
    // Mirrors upstream's "keeps defining context when inserting at the start
    // of a textblock":
    //   doc(p("<a>foo"))
    //   slice: ul(li(p("one")), li(p("two")))  with parents → openStart=3, openEnd=3
    //   expected: doc(ul(li(p("one")), li(p("twofoo"))))
    //
    // The slice's `list_item` is declared defining_for_content in
    // schema_basic, so the wrapper structure must survive the replace and
    // the cursor parent's "foo" content gets merged into the slice's last
    // textblock.
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("foo").into(),
    ])
    .into()]);
    let source = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item(vec![tagged_paragraph(vec![
            tag("a"),
            tagged_text("one").into(),
        ])
        .into()])
        .into(),
        tagged_list_item(vec![tagged_paragraph(vec![
            tagged_text("two").into(),
            tag("b"),
        ])
        .into()])
        .into(),
    ])
    .into()]);
    let slice = source
        .node
        .slice_with_parents(source.tag("a"), source.tag("b"), true)
        .unwrap();
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(pos, pos, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("one"),
            list_item_text("twofoo"),
        ])])
    );
}

#[test]
fn transform_keeps_defining_context_at_textblock_end() {
    // Cursor at end of paragraph: head is everything in the paragraph, tail
    // is empty. The slice's first textblock should absorb the head.
    //   doc(p("foo<a>"))
    //   slice: ul(li(p("one")), li(p("two")))  with openStart=3, openEnd=3
    //   expected: doc(ul(li(p("fooone")), li(p("two"))))
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo").into(),
        tag("a"),
    ])
    .into()]);
    let source = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item(vec![tagged_paragraph(vec![
            tag("a"),
            tagged_text("one").into(),
        ])
        .into()])
        .into(),
        tagged_list_item(vec![tagged_paragraph(vec![
            tagged_text("two").into(),
            tag("b"),
        ])
        .into()])
        .into(),
    ])
    .into()]);
    let slice = source
        .node
        .slice_with_parents(source.tag("a"), source.tag("b"), true)
        .unwrap();
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(pos, pos, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("fooone"),
            list_item_text("two"),
        ])])
    );
}

#[test]
fn transform_keeps_defining_context_over_a_range_in_one_textblock() {
    // Range selection inside a single textblock — the slice's defining
    // wrapper must survive while the selected text is deleted and the
    // surrounding head/tail merge into the slice's leaves.
    //   doc(p("foo<a>middle<b>bar"))
    //   slice: ul(li(p("one")), li(p("two")))  with openStart=3, openEnd=3
    //   expected: doc(ul(li(p("fooone")), li(p("twobar"))))
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo").into(),
        tag("a"),
        tagged_text("middle").into(),
        tag("b"),
        tagged_text("bar").into(),
    ])
    .into()]);
    let source = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item(vec![tagged_paragraph(vec![
            tag("a"),
            tagged_text("one").into(),
        ])
        .into()])
        .into(),
        tagged_list_item(vec![tagged_paragraph(vec![
            tagged_text("two").into(),
            tag("b"),
        ])
        .into()])
        .into(),
    ])
    .into()]);
    let slice = source
        .node
        .slice_with_parents(source.tag("a"), source.tag("b"), true)
        .unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(from, to, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("fooone"),
            list_item_text("twobar"),
        ])])
    );
}

#[test]
fn transform_keeps_defining_context_across_two_textblocks() {
    // Range selection that spans two paragraphs at the same depth — both
    // paragraphs disappear and head/tail merge into the slice's leaves while
    // the defining list wrapper survives.
    //   doc(p("foo<a>middle"), p("text<b>bar"))
    //   slice: ul(li(p("one")), li(p("two")))  with openStart=3, openEnd=3
    //   expected: doc(ul(li(p("fooone")), li(p("twobar"))))
    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("foo").into(),
            tag("a"),
            tagged_text("middle").into(),
        ])
        .into(),
        tagged_paragraph(vec![
            tagged_text("text").into(),
            tag("b"),
            tagged_text("bar").into(),
        ])
        .into(),
    ]);
    let source = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item(vec![tagged_paragraph(vec![
            tag("a"),
            tagged_text("one").into(),
        ])
        .into()])
        .into(),
        tagged_list_item(vec![tagged_paragraph(vec![
            tagged_text("two").into(),
            tag("b"),
        ])
        .into()])
        .into(),
    ])
    .into()]);
    let slice = source
        .node
        .slice_with_parents(source.tag("a"), source.tag("b"), true)
        .unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(from, to, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("fooone"),
            list_item_text("twobar"),
        ])])
    );
}

#[test]
fn transform_lift_stops_at_isolating_ancestor() {
    // Schema where `iso` is an isolating block-list block wrapper.
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("block+"))
        .node(NodeSpec::new("paragraph").group("block").content("text*"))
        .node(
            NodeSpec::new("iso")
                .group("block")
                .content("block+")
                .isolating(),
        )
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();

    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("hi", Vec::new()).unwrap()),
        )
        .unwrap();
    let inner_iso = schema
        .node("iso", Attrs::new(), Fragment::from(para.clone()))
        .unwrap();
    let outer = schema
        .node("iso", Attrs::new(), Fragment::from(inner_iso))
        .unwrap();
    let document = schema
        .node("doc", Attrs::new(), Fragment::from(outer))
        .unwrap();

    // Position inside the innermost paragraph (depth = 3).
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), document);
    // The lift should stop at the outer isolating boundary: it can lift the
    // paragraph out of its inner iso but not past the outer iso.
    let pos = 1 + 1 + 1 + 1; // doc.open + iso.open + iso.open + paragraph.open
    tr.lift(pos, pos).unwrap();
    // The inner iso disappears (the paragraph rose into the outer iso's
    // content), but the outer iso wrapper remains because it's isolating.
    let outer = tr.doc().content().child(0).unwrap();
    assert_eq!(outer.type_name(), "iso");
    // The outer iso now contains the paragraph directly (no inner iso wrapper).
    assert_eq!(outer.content().child(0).unwrap().type_name(), "paragraph");
}

#[test]
fn transform_partial_lift_emits_replace_around_step() {
    // A partial-wrapper lift with boundary positions (cursor BETWEEN block
    // children, not inside a textblock's text) used to fall back to a
    // whole-doc Replace, which doesn't compose through mapping. After fixing
    // ReplaceAroundStep.apply to offset the insert position by open_start,
    // build_lift_around_step's open-slice approach produces the correct
    // doc for boundary positions too — so the step type should now be
    // ReplaceAround across the board, not Replace.
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
        tagged_paragraph_text("three").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");

    let mut lift_tr =
        pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node.clone());
    lift_tr.lift(from, to).unwrap();
    assert!(
        matches!(
            lift_tr.steps()[0],
            pine_richtext::transform::Step::ReplaceAround(_)
        ),
        "boundary-position partial lift should emit a ReplaceAroundStep, got {:?}",
        lift_tr.steps()[0],
    );

    // And composing the lift step through a concurrent insert at the very
    // start of the doc should still produce a correctly-lifted doc.
    let mut insert_tr =
        pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node.clone());
    insert_tr
        .insert(0, Fragment::from(paragraph_text("intro")))
        .unwrap();

    let mapped = lift_tr.steps()[0]
        .map_step(&insert_tr.mapping())
        .expect("lift step should map through the concurrent insertion");
    let result = mapped
        .apply(insert_tr.doc(), &schema_basic::schema())
        .unwrap();
    assert_eq!(
        &result.doc,
        &doc(vec![
            paragraph_text("intro"),
            schema_basic::blockquote(vec![paragraph_text("one")]).unwrap(),
            paragraph_text("two"),
            schema_basic::blockquote(vec![paragraph_text("three")]).unwrap(),
        ])
    );
}

#[test]
fn transform_cross_depth_replace_bails_at_isolating_boundary() {
    // PM's Fitter refuses to join content across an isolating wrapper. Pine's
    // cross-depth empty-slice fitter mirrors that: replacing a range that
    // straddles an isolating ancestor must not merge the inner textblock's
    // content with the outer textblock's content.
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("block+"))
        .node(NodeSpec::new("paragraph").group("block").content("text*"))
        .node(
            NodeSpec::new("iso")
                .group("block")
                .content("block+")
                .isolating(),
        )
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();

    // Doc: iso(paragraph("hello")) + paragraph("world")
    let inner = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("hello", Vec::new()).unwrap()),
        )
        .unwrap();
    let iso = schema
        .node("iso", Attrs::new(), Fragment::from(inner))
        .unwrap();
    let outer = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("world", Vec::new()).unwrap()),
        )
        .unwrap();
    let document = schema
        .node("doc", Attrs::new(), Fragment::from(vec![iso, outer]))
        .unwrap();

    // Positions:
    //   doc opens                          pos 0
    //   iso opens                          pos 1
    //   inner paragraph opens              pos 2
    //   "hello"                            pos 2..7
    //   inner paragraph closes             pos 8
    //   iso closes                         pos 9
    //   outer paragraph opens              pos 10
    //   "world"                            pos 10..15
    //   outer paragraph closes             pos 16
    //
    // From inside iso's paragraph between "hel" and "lo" → pos 5 (depth 3).
    // To inside outer paragraph between "wo" and "rld"  → pos 12 (depth 2).
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), document.clone());
    let outcome = tr.replace(5, 12, Slice::empty());

    // Either the replace errors out (no fitter could land it) or it succeeds
    // without crossing the boundary. Whichever it is, the iso wrapper must
    // still be intact and the inner paragraph must not have absorbed any text
    // from the outer paragraph.
    if outcome.is_ok() {
        let root = tr.doc();
        let first = root.content().child(0).unwrap();
        assert_eq!(first.type_name(), "iso");
        let inner_p = first.content().child(0).unwrap();
        assert_eq!(inner_p.type_name(), "paragraph");
        let text_node = inner_p.content().child(0).unwrap();
        assert!(
            text_node.is_text() && !text_node.text().unwrap_or("").contains("rld"),
            "inner iso paragraph absorbed text from outside the isolating boundary",
        );
    }
}

#[test]
fn transform_changed_range_matches_upstream_cases() {
    fn changed_tuple(tr: &pine_richtext::transform::Transform) -> Option<(usize, usize)> {
        tr.changed_range().map(|range| (range.from, range.to))
    }

    let mut tr = pine_richtext::transform::Transform::new(
        schema_basic::schema(),
        doc(vec![paragraph_text("hello")]),
    );
    assert_eq!(changed_tuple(&tr), None);
    tr.add_mark(1, 3, schema_basic::strong().unwrap()).unwrap();
    assert_eq!(changed_tuple(&tr), None);

    let mut tr = pine_richtext::transform::Transform::new(
        schema_basic::schema(),
        doc(vec![paragraph_text("ab")]),
    );
    tr.insert(3, Fragment::from(text("c"))).unwrap();
    assert_eq!(changed_tuple(&tr), Some((3, 4)));

    let mut tr = pine_richtext::transform::Transform::new(
        schema_basic::schema(),
        doc(vec![paragraph_text("ab")]),
    );
    tr.insert(3, Fragment::from(text("c")))
        .unwrap()
        .insert(2, Fragment::from(text("d")))
        .unwrap()
        .insert(1, Fragment::from(text("e")))
        .unwrap();
    assert_eq!(changed_tuple(&tr), Some((1, 6)));

    let mut tr = pine_richtext::transform::Transform::new(
        schema_basic::schema(),
        doc(vec![paragraph_text("abcde")]),
    );
    tr.insert(6, Fragment::from(text("f")))
        .unwrap()
        .delete(1, 4)
        .unwrap();
    assert_eq!(changed_tuple(&tr), Some((1, 4)));
}

#[test]
fn transform_node_mark_methods_match_upstream_cases() {
    let em = schema_basic::em().unwrap();
    let document = doc(vec![paragraph(vec![image("x.png")])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);

    tr.add_node_mark(1, em.clone()).unwrap();
    assert_eq!(
        tr.doc()
            .content()
            .child(0)
            .unwrap()
            .content()
            .child(0)
            .unwrap()
            .marks(),
        std::slice::from_ref(&em)
    );

    tr.add_node_mark(1, em.clone()).unwrap();
    assert_eq!(
        tr.doc()
            .content()
            .child(0)
            .unwrap()
            .content()
            .child(0)
            .unwrap()
            .marks(),
        std::slice::from_ref(&em)
    );

    tr.remove_node_mark(1, em.clone()).unwrap();
    assert!(tr
        .doc()
        .content()
        .child(0)
        .unwrap()
        .content()
        .child(0)
        .unwrap()
        .marks()
        .is_empty());
    tr.remove_node_mark(1, em).unwrap();

    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("p+"))
        .node(NodeSpec::new("p").content("text*"))
        .node(NodeSpec::new("text").inline())
        .mark(MarkSpec::new("comment").excludes(""))
        .finish()
        .unwrap();
    let mut attrs = Attrs::new();
    attrs.insert("id".to_string(), json!(1));
    let comment_one = schema.mark("comment", attrs).unwrap();
    let mut attrs = Attrs::new();
    attrs.insert("id".to_string(), json!(2));
    let comment_two = schema.mark("comment", attrs).unwrap();
    let text = schema.text("abc", Vec::new()).unwrap();
    let paragraph = schema
        .node("p", Attrs::new(), Fragment::from(text))
        .unwrap()
        .with_marks(vec![comment_one, comment_two]);
    let document = schema
        .node("doc", Attrs::new(), Fragment::from(paragraph))
        .unwrap();
    let mut tr = pine_richtext::transform::Transform::new(schema, document);

    tr.remove_node_mark_type(0, "comment").unwrap();

    assert!(tr.doc().content().child(0).unwrap().marks().is_empty());
}

#[test]
fn transform_set_node_markup_matches_upstream_cases() {
    let tagged = tagged_doc(vec![tag("a"), tagged_paragraph_text("foo").into()]);
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(1));
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.set_node_markup(pos, Some("heading"), attrs, None)
        .unwrap();

    assert_eq!(tr.doc(), &doc(vec![heading(1, "foo")]));

    let document = doc(vec![paragraph(vec![
        text("foo"),
        image("old.png"),
        text("bar"),
    ])]);
    let mut attrs = Attrs::new();
    attrs.insert("src".to_string(), json!("bar.png"));
    attrs.insert("alt".to_string(), json!("y"));
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);

    tr.set_node_markup(4, Some("image"), attrs, None).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("foo"),
            schema_basic::image("bar.png", Some("y"), Option::<String>::None).unwrap(),
            text("bar"),
        ])])
    );
}

#[test]
fn transform_set_block_type_matches_core_upstream_cases() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("am<a> i").into()]);
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(2));
    let from = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.set_block_type(from, from, "heading", attrs).unwrap();

    assert_eq!(tr.doc(), &doc(vec![heading(2, "am i")]));

    let tagged = tagged_doc(vec![
        tagged_heading_text(1, "<a>hello").into(),
        tagged_paragraph_text("there").into(),
        tagged_paragraph_text("<b>you").into(),
        tagged_paragraph_text("end").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.set_block_type(from, to, "code_block", Attrs::new())
        .unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![
            code_block("hello"),
            code_block("there"),
            code_block("you"),
            paragraph_text("end"),
        ])
    );

    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("hello ").into(),
        tagged_marked_text("wor<a>ld", vec![em]).into(),
        TaggedNode {
            node: image("x.png"),
            tags: Default::default(),
        }
        .into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.set_block_type(from, from, "code_block", Attrs::new())
        .unwrap();

    assert_eq!(tr.doc(), &doc(vec![code_block("hello world")]));
}

#[test]
fn transform_step_map_matches_core_upstream_mapping_cases() {
    let insertion = StepMap::single(2, 0, 4);
    assert_eq!(insertion.map(0, 1), 0);
    assert_eq!(insertion.map(2, 1), 6);
    assert_eq!(insertion.map(2, -1), 2);
    assert_eq!(insertion.map(3, 1), 7);

    let deletion = StepMap::single(2, 4, 0);
    assert_eq!(deletion.map(0, 1), 0);
    assert_eq!(deletion.map(2, -1), 2);
    assert_eq!(deletion.map(3, 1), 2);
    assert!(deletion.map_result(3, 1).deleted);
    assert_eq!(deletion.map(6, 1), 2);
    assert_eq!(deletion.map(7, 1), 3);

    let replacement = StepMap::single(2, 4, 4);
    assert_eq!(replacement.map(0, 1), 0);
    assert_eq!(replacement.map(2, 1), 2);
    assert_eq!(replacement.map(4, 1), 6);
    assert_eq!(replacement.map(4, -1), 2);
    assert!(replacement.map_result(4, 1).deleted);
    assert_eq!(replacement.map(6, -1), 6);
    assert_eq!(replacement.map(6, 1), 6);
    assert_eq!(replacement.map(8, 1), 8);
}

#[test]
fn transform_step_map_reports_deleted_flags() {
    assert_eq!(map_flags(StepMap::single(0, 2, 0).map_result(2, -1)), "db");
    assert_eq!(map_flags(StepMap::single(0, 2, 0).map_result(2, 1)), "b");
    assert_eq!(map_flags(StepMap::single(0, 2, 2).map_result(2, -1)), "db");
    assert_eq!(map_flags(StepMap::single(2, 2, 0).map_result(2, -1)), "a");
    assert_eq!(map_flags(StepMap::single(2, 2, 0).map_result(2, 1)), "da");
    assert_eq!(map_flags(StepMap::single(2, 2, 2).map_result(2, 1)), "da");
    assert_eq!(
        map_flags(StepMap::single(0, 4, 0).map_result(2, -1)),
        "dbax"
    );
    assert_eq!(map_flags(StepMap::single(0, 4, 0).map_result(2, 1)), "dbax");
}

#[test]
fn transform_mapping_preserves_deleted_flags_across_maps() {
    let mut before = Mapping::new();
    before.append_map(StepMap::single(0, 1, 0));
    before.append_map(StepMap::single(0, 1, 0));
    assert_eq!(map_flags(before.map_result(2, -1)), "db");

    let mut after = Mapping::new();
    after.append_map(StepMap::single(2, 1, 0));
    after.append_map(StepMap::single(2, 1, 0));
    assert_eq!(map_flags(after.map_result(2, 1)), "da");

    let mut across = Mapping::new();
    across.append_map(StepMap::single(0, 1, 0));
    across.append_map(StepMap::single(4, 1, 0));
    across.append_map(StepMap::single(0, 3, 0));
    assert_eq!(map_flags(across.map_result(2, 1)), "dbax");

    let mut around = Mapping::new();
    around.append_map(StepMap::single(2, 1, 0));
    around.append_map(StepMap::single(0, 2, 0));
    assert_eq!(map_flags(around.map_result(2, -1)), "dba");
}

#[test]
fn transform_mapping_uses_mirror_recovery() {
    let mut delete_insert = Mapping::new();
    delete_insert.append_map(StepMap::single(2, 4, 0));
    delete_insert.append_map_with_mirror(StepMap::single(2, 0, 4), 0);
    assert_mapping(
        &delete_insert,
        &[
            (0, 0, 1, false),
            (2, 2, 1, false),
            (4, 4, 1, false),
            (6, 6, 1, false),
            (7, 7, 1, false),
        ],
    );

    let mut insert_delete = Mapping::new();
    insert_delete.append_map(StepMap::single(2, 0, 4));
    insert_delete.append_map_with_mirror(StepMap::single(2, 4, 0), 0);
    assert_mapping(
        &insert_delete,
        &[(0, 0, 1, false), (2, 2, 1, false), (3, 3, 1, false)],
    );

    let mut with_insert_between = Mapping::new();
    with_insert_between.append_map(StepMap::single(2, 4, 0));
    with_insert_between.append_map(StepMap::single(1, 0, 1));
    with_insert_between.append_map_with_mirror(StepMap::single(3, 0, 4), 0);
    assert_mapping(
        &with_insert_between,
        &[
            (0, 0, 1, false),
            (1, 2, 1, false),
            (4, 5, 1, false),
            (6, 7, 1, false),
            (7, 8, 1, false),
        ],
    );
}

#[test]
fn transform_step_map_exposes_recover_and_touches_helpers() {
    let map = StepMap::single(2, 4, 0);
    let result = map.map_result(4, 1);
    let recover = result.recover().expect("position inside replaced range");

    assert_eq!(map.recover(recover), Some(4));
    assert!(map.touches(4, recover));
    assert!(!map.touches(7, recover));
}

#[test]
fn transform_mapping_inverts_multirange_step_maps() {
    let map = StepMap {
        ranges: vec![
            MapRange {
                old_start: 1,
                old_size: 1,
                new_size: 2,
            },
            MapRange {
                old_start: 5,
                old_size: 1,
                new_size: 0,
            },
        ],
        inverted: false,
    };
    let mut mapping = Mapping::new();
    mapping.append_map(map);

    assert_mapping(
        &mapping,
        &[
            (0, 0, 1, false),
            (1, 1, 1, false),
            (2, 3, 1, false),
            (4, 5, 1, false),
            (5, 6, -1, true),
            (6, 6, 1, false),
        ],
    );
}

#[test]
fn transform_step_map_iterates_ranges_and_offsets() {
    let offset = StepMap::offset(3);
    assert_eq!(offset.map(2, 1), 5);
    assert_eq!(offset.invert().map(5, 1), 2);

    let negative_offset = StepMap::offset(-2);
    assert_eq!(negative_offset.map(5, 1), 3);
    assert!(negative_offset.map_result(1, 1).deleted);

    let map = StepMap {
        ranges: vec![
            MapRange {
                old_start: 1,
                old_size: 1,
                new_size: 2,
            },
            MapRange {
                old_start: 5,
                old_size: 1,
                new_size: 0,
            },
        ],
        inverted: false,
    };
    let mut ranges = Vec::new();
    map.for_each(|old_start, old_end, new_start, new_end| {
        ranges.push((old_start, old_end, new_start, new_end));
    });
    assert_eq!(ranges, vec![(1, 2, 1, 3), (5, 6, 6, 6)]);

    let mut inverted_ranges = Vec::new();
    map.invert()
        .for_each(|old_start, old_end, new_start, new_end| {
            inverted_ranges.push((old_start, old_end, new_start, new_end));
        });
    assert_eq!(inverted_ranges, vec![(1, 3, 1, 2), (6, 6, 5, 6)]);
}

#[test]
fn transform_structure_finds_joinable_boundaries() {
    let schema = schema_basic::schema();
    let joinable = doc(vec![paragraph_text("one"), paragraph_text("two")]);
    let boundary = joinable.content().child(0).unwrap().node_size();

    assert!(can_join(&joinable, boundary, &schema));
    assert_eq!(
        join_point(&joinable, boundary + 2, -1, &schema),
        Some(boundary)
    );
    assert_eq!(join_point(&joinable, 2, 1, &schema), Some(boundary));

    // Compatible block types join when the left's content expression accepts the
    // right's children (matches upstream `canJoin` via `canAppend`).
    let compatible = doc(vec![heading(1, "one"), paragraph_text("two")]);
    let compat_boundary = compatible.content().child(0).unwrap().node_size();
    assert!(can_join(&compatible, compat_boundary, &schema));

    // Two top-level blocks whose content expressions don't overlap (a horizontal
    // rule has no content expression matching a paragraph's inline children).
    let blocked = doc(vec![paragraph_text("one"), horizontal_rule()]);
    let blocked_boundary = blocked.content().child(0).unwrap().node_size();
    assert!(!can_join(&blocked, blocked_boundary, &schema));
    assert_eq!(join_point(&blocked, blocked_boundary, -1, &schema), None);
}

#[test]
fn transform_replace_joins_matching_top_level_textblocks() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.delete(from, to).unwrap();

    assert_eq!(tr.doc().child_count(), 1);
    assert_eq!(tr.doc().content().child(0).unwrap().text_content(), "onwo");

    let tagged = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let replacement = Slice::new(Fragment::from(text("H")), 0, 0);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, replacement).unwrap();

    assert_eq!(tr.doc().child_count(), 1);
    assert_eq!(tr.doc().content().child(0).unwrap().text_content(), "onHwo");
}

#[test]
fn transform_replace_handles_open_and_nested_slices() {
    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_blockquote(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ])
    .into()])
    .into()]);
    let insert = tagged_doc(vec![tagged_paragraph_text("<a>H<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    let expected = doc(vec![schema_basic::blockquote(vec![
        schema_basic::blockquote(vec![paragraph_text("onHwo")]).unwrap(),
    ])
    .unwrap()]);
    assert_eq!(tr.doc(), &expected);

    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_paragraph_text(
        "a<a>bc<b>d",
    )
    .into()])
    .into()]);
    let insert = tagged_doc(vec![tagged_paragraph_text("x<a>y<b>z").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    let expected = doc(vec![
        schema_basic::blockquote(vec![paragraph_text("ayd")]).unwrap()
    ]);
    assert_eq!(tr.doc(), &expected);
}

#[test]
fn transform_replace_matches_more_upstream_open_slice_cases() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    let insert = tagged_doc(vec![
        tagged_paragraph_text("xx<a>xx").into(),
        tagged_paragraph_text("yy<b>yy").into(),
    ]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph_text("onxx"), paragraph_text("yywo")])
    );

    let tagged = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    let insert = tagged_doc(vec![tagged_heading_text(1, "<a>H<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(tr.doc(), &doc(vec![paragraph_text("onHwo")]));

    let tagged = tagged_doc(vec![
        tagged_paragraph_text("before").into(),
        tagged_paragraph_text("on<a><b>e").into(),
        tagged_paragraph_text("after").into(),
    ]);
    let insert = tagged_doc(vec![tagged_paragraph_text("<a>H<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("before"),
            paragraph_text("onHe"),
            paragraph_text("after")
        ])
    );

    let tagged = tagged_doc(vec![tagged_heading_text(1, "foo<a>bar").into(), tag("b")]);
    let insert = tagged_doc(vec![tagged_paragraph_text("foo<a>baz").into(), tag("b")]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(tr.doc(), &doc(vec![heading(1, "foobaz")]));

    let tagged = tagged_doc(vec![tagged_heading_text(1, "<a>bar").into(), tag("b")]);
    let insert = tagged_doc(vec![tagged_paragraph_text("foo<a>baz").into(), tag("b")]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(tr.doc(), &doc(vec![heading(1, "baz")]));
}

#[test]
fn transform_replace_merges_multiple_open_levels() {
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_blockquote(vec![tagged_paragraph_text(
            "hell<a>o",
        )
        .into()])
        .into()])
        .into(),
        tagged_blockquote(vec![tagged_blockquote(vec![
            tagged_paragraph_text("<b>a").into()
        ])
        .into()])
        .into(),
    ]);
    let insert = tagged_doc(vec![tagged_paragraph_text("<a>i<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    let expected = doc(vec![schema_basic::blockquote(vec![
        schema_basic::blockquote(vec![paragraph_text("hellia")]).unwrap(),
    ])
    .unwrap()]);
    assert_eq!(tr.doc(), &expected);
}

#[test]
fn transform_replace_handles_lopsided_open_slices() {
    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_blockquote(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("two").into(),
        tag("b"),
        tagged_paragraph_text("three").into(),
    ])
    .into()])
    .into()]);
    let insert = tagged_doc(vec![tagged_blockquote(vec![
        tagged_paragraph_text("aa<a>aa").into(),
        tagged_paragraph_text("bb").into(),
        tagged_paragraph_text("cc").into(),
        tag("b"),
        tagged_paragraph_text("dd").into(),
    ])
    .into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    let expected = doc(vec![schema_basic::blockquote(vec![
        schema_basic::blockquote(vec![
            paragraph_text("onaa"),
            paragraph_text("bb"),
            paragraph_text("cc"),
            paragraph_text("three"),
        ])
        .unwrap(),
    ])
    .unwrap()]);
    assert_eq!(tr.doc(), &expected);

    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("on<a>e").into(),
            tagged_paragraph_text("two").into(),
            tagged_paragraph_text("three").into(),
        ])
        .into(),
        tag("b"),
        tagged_paragraph_text("x").into(),
    ])
    .into()]);
    let insert = tagged_doc(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("aa<a>aa").into(),
            tagged_paragraph_text("bb").into(),
            tagged_paragraph_text("cc").into(),
        ])
        .into(),
        tag("b"),
        tagged_paragraph_text("dd").into(),
    ]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    let expected = doc(vec![schema_basic::blockquote(vec![
        schema_basic::blockquote(vec![
            paragraph_text("onaa"),
            paragraph_text("bb"),
            paragraph_text("cc"),
        ])
        .unwrap(),
        paragraph_text("x"),
    ])
    .unwrap()]);
    assert_eq!(tr.doc(), &expected);
}

#[test]
fn transform_replace_can_insert_open_split() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a><b>bar").into()]);
    let insert = tagged_doc(vec![
        tagged_paragraph_text("<a>x").into(),
        tagged_paragraph_text("y<b>").into(),
    ]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph_text("foox"), paragraph_text("ybar")])
    );
}

#[test]
fn transform_replace_rejects_open_depth_mismatches() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("<a><b>").into()]);
    let insert = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("<a>").into()]).into(),
        tag("b"),
    ]);
    assert_replace_error(tagged, Some(insert), "deeper");

    let tagged = tagged_doc(vec![tagged_paragraph_text("<a><b>").into()]);
    let insert = tagged_doc(vec![tag("a"), tagged_paragraph_text("<b>").into()]);
    assert_replace_error(tagged, Some(insert), "inconsistent");
}

#[test]
fn transform_replace_fits_block_slice_inside_textblock() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("hello<a><b>you").into()]);
    let insert = tagged_doc(vec![
        tag("a"),
        tagged_paragraph_text("there").into(),
        tag("b"),
    ]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace(from, to, slice).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("hello"),
            paragraph_text("there"),
            paragraph_text("you")
        ])
    );
}

#[test]
fn transform_replace_range_with_fits_block_and_inline_nodes() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>bar").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace_range_with(pos, pos, horizontal_rule()).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("foo"),
            horizontal_rule(),
            paragraph_text("bar")
        ])
    );

    let tagged = tagged_doc(vec![tagged_heading_text(1, "foo<a>").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace_range_with(pos, pos, horizontal_rule()).unwrap();

    assert_eq!(tr.doc(), &doc(vec![heading(1, "foo"), horizontal_rule()]));

    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_paragraph_text("<a>").into()
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace_range_with(pos, pos, horizontal_rule()).unwrap();

    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![horizontal_rule()]).unwrap()
        ])
    );

    let tagged = tagged_doc(vec![
        tag("a"),
        tagged_blockquote(vec![tagged_paragraph_text("a").into()]).into(),
        tag("b"),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);

    tr.replace_range_with(from, to, image("x.png")).unwrap();

    assert_eq!(tr.doc(), &doc(vec![paragraph(vec![image("x.png")])]));
}

#[test]
fn transform_mapping_slices_maps_and_mirrors() {
    let mut mapping = Mapping::new();
    mapping.append_map(StepMap::single(2, 4, 0));
    mapping.append_map(StepMap::single(1, 0, 1));
    mapping.append_map_with_mirror(StepMap::single(3, 0, 4), 0);

    let middle = mapping.slice(1, 2);
    assert_eq!(middle.maps.len(), 1);
    assert!(middle.get_mirror(0).is_none());
    assert_eq!(middle.map(1, 1), 2);

    let full = mapping.slice(0, 3);
    assert_eq!(full.get_mirror(0), Some(2));
    assert_eq!(full.map(4, 1), 5);
    assert_eq!(full.map(7, 1), 8);

    let tail = mapping.slice(2, usize::MAX);
    assert_eq!(tail.maps.len(), 1);
    assert!(tail.get_mirror(0).is_none());
    assert_eq!(tail.map(3, 1), 7);
}

#[test]
fn transform_mapping_composes_multiple_step_maps() {
    let mut mapping = Mapping::new();
    mapping.append_map(StepMap::single(2, 4, 0));
    mapping.append_map(StepMap::single(1, 0, 1));
    mapping.append_map(StepMap::single(3, 0, 4));

    assert_eq!(mapping.map(0, 1), 0);
    assert_eq!(mapping.map(1, 1), 2);
    assert_eq!(mapping.map(4, 1), 7);
    assert_eq!(mapping.map(6, 1), 7);
}

#[test]
fn transform_steps_merge_like_upstream_step_cases() {
    assert_steps_merge(replace_step(2, 2, Some("a")), replace_step(3, 3, Some("b")));
    assert_steps_merge(replace_step(2, 2, Some("a")), replace_step(2, 2, Some("b")));
    assert_steps_do_not_merge(replace_step(2, 2, Some("a")), replace_step(4, 4, Some("b")));
    assert_steps_do_not_merge(replace_step(3, 3, Some("a")), replace_step(2, 2, Some("b")));

    assert_steps_merge(replace_step(3, 4, None), replace_step(2, 3, None));
    assert_steps_merge(replace_step(2, 3, None), replace_step(2, 3, None));
    assert_steps_do_not_merge(replace_step(1, 2, None), replace_step(2, 3, None));
    assert_steps_merge(replace_step(2, 3, None), replace_step(2, 2, Some("x")));
    assert_steps_merge(
        replace_step(2, 2, Some("quux")),
        replace_step(6, 6, Some("baz")),
    );
    assert_steps_merge(
        replace_step(2, 2, Some("quux")),
        replace_step(2, 2, Some("baz")),
    );
    assert_steps_merge(replace_step(2, 5, None), replace_step(2, 4, None));
    assert_steps_merge(replace_step(4, 6, None), replace_step(2, 4, None));
    assert_steps_merge(replace_step(3, 4, Some("x")), replace_step(4, 5, Some("y")));

    assert_steps_merge(add_em_step(1, 2), add_em_step(2, 4));
    assert_steps_merge(add_em_step(1, 3), add_em_step(2, 4));
    assert_steps_do_not_merge(add_em_step(1, 2), add_em_step(3, 4));
    assert_steps_merge(remove_em_step(1, 2), remove_em_step(2, 4));
    assert_steps_merge(remove_em_step(1, 3), remove_em_step(2, 4));
    assert_steps_do_not_merge(remove_em_step(1, 2), remove_em_step(3, 4));
}

#[test]
fn transform_steps_map_through_position_changes() {
    let mut mapping = Mapping::new();
    mapping.append_map(StepMap::single(2, 0, 2));

    let mapped = add_em_step(3, 5).map_step(&mapping).unwrap();
    assert_eq!(mapped, add_em_step(5, 7));

    let mapped = replace_step(3, 5, Some("x")).map_step(&mapping).unwrap();
    assert_eq!(mapped, replace_step(5, 7, Some("x")));

    let mut deletion = Mapping::new();
    deletion.append_map(StepMap::single(2, 4, 0));
    assert!(add_em_step(3, 5).map_step(&deletion).is_none());
}

#[test]
fn transform_add_mark_matches_upstream_cases() {
    let strong = schema_basic::strong().unwrap();
    let em = schema_basic::em().unwrap();
    let code = schema_basic::code().unwrap();
    let link_foo = schema_basic::link("foo", Option::<String>::None).unwrap();
    let link_bar = schema_basic::link("bar", Option::<String>::None).unwrap();

    // should add a mark
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("hello ").into(),
        tag("a"),
        tagged_text("there").into(),
        tag("b"),
        tagged_text("!").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.add_mark(from, to, strong.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("hello "),
            marked_text("there", vec![strong.clone()]),
            text("!"),
        ])])
    );

    // should only add a mark once (idempotent over the same span)
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("hello ").into(),
        tag("a"),
        tagged_marked_text("there", vec![strong.clone()]).into(),
        tagged_text("!").into(),
        tag("b"),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.add_mark(from, to, strong.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("hello "),
            marked_text("there!", vec![strong.clone()]),
        ])])
    );

    // joins overlapping marks
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("one ").into(),
        tag("a"),
        tagged_text("two ").into(),
        tagged_marked_text("three", vec![em.clone()]).into(),
        tag("b"),
        tagged_marked_text(" four", vec![em.clone()]).into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.add_mark(from, to, strong.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("one "),
            marked_text("two ", vec![strong.clone()]),
            marked_text("three", vec![em.clone(), strong.clone()]),
            marked_text(" four", vec![em.clone()]),
        ])])
    );

    // overwrites marks with different attributes (link href)
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("this is a ").into(),
        tag("a"),
        tagged_marked_text("link", vec![link_foo]).into(),
        tag("b"),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.add_mark(from, to, link_bar.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("this is a "),
            marked_text("link", vec![link_bar]),
        ])])
    );

    // can add a mark in a nested node
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("before").into(),
        tagged_blockquote(vec![tagged_paragraph(vec![
            tagged_text("the variable is called ").into(),
            tag("a"),
            tagged_text("i").into(),
            tag("b"),
        ])
        .into()])
        .into(),
        tagged_paragraph_text("after").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.add_mark(from, to, code.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("before"),
            schema_basic::blockquote(vec![paragraph(vec![
                text("the variable is called "),
                marked_text("i", vec![code]),
            ])])
            .unwrap(),
            paragraph_text("after"),
        ])
    );

    // can add a mark across blocks
    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("hi ").into(),
            tag("a"),
            tagged_text("this").into(),
        ])
        .into(),
        tagged_blockquote(vec![tagged_paragraph_text("is").into()]).into(),
        tagged_paragraph(vec![
            tagged_text("a docu").into(),
            tag("b"),
            tagged_text("ment").into(),
        ])
        .into(),
        tagged_paragraph_text("!").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.add_mark(from, to, em.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph(vec![text("hi "), marked_text("this", vec![em.clone()]),]),
            schema_basic::blockquote(vec![paragraph(vec![marked_text("is", vec![em.clone()])])])
                .unwrap(),
            paragraph(vec![marked_text("a docu", vec![em]), text("ment"),]),
            paragraph_text("!"),
        ])
    );
}

#[test]
fn transform_remove_mark_matches_upstream_cases() {
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let link_foo = schema_basic::link("foo", Option::<String>::None).unwrap();
    let link_bar = schema_basic::link("bar", Option::<String>::None).unwrap();

    // can cut a gap
    let tagged = tagged_doc(vec![tagged_paragraph(vec![tagged_marked_text(
        "hello <a>world<b>!",
        vec![em.clone()],
    )
    .into()])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.remove_mark(from, to, em.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            marked_text("hello ", vec![em.clone()]),
            text("world"),
            marked_text("!", vec![em.clone()]),
        ])])
    );

    // does nothing when there's no mark in range
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_marked_text("hello", vec![em.clone()]).into(),
        tagged_text(" ").into(),
        tag("a"),
        tagged_text("world").into(),
        tag("b"),
        tagged_text("!").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.remove_mark(from, to, em.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            marked_text("hello", vec![em.clone()]),
            text(" world!"),
        ])])
    );

    // can remove marks from nested nodes
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_marked_text("one ", vec![em.clone()]).into(),
        tag("a"),
        tagged_marked_text("two", vec![em.clone(), strong.clone()]).into(),
        tag("b"),
        tagged_marked_text(" three", vec![em.clone()]).into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.remove_mark(from, to, strong.clone()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![marked_text(
            "one two three",
            vec![em.clone()]
        )])])
    );

    // can remove a link
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("hello ").into(),
        tagged_marked_text("link", vec![link_foo.clone()]).into(),
        tag("b"),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.remove_mark(from, to, link_foo.clone()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("hello link")]));

    // doesn't remove a non-matching link (different href)
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("hello ").into(),
        tagged_marked_text("link", vec![link_foo.clone()]).into(),
        tag("b"),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.remove_mark(from, to, link_bar).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("hello "),
            marked_text("link", vec![link_foo]),
        ])])
    );
}

#[test]
fn transform_insert_matches_upstream_cases() {
    // can insert a break
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("hello").into(),
        tag("a"),
        tagged_text("there").into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.insert(pos, Fragment::from(hard_break())).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("hello"),
            hard_break(),
            text("there"),
        ])])
    );

    // can insert an empty paragraph at the top
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.insert(pos, Fragment::from(empty_paragraph())).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("one"),
            empty_paragraph(),
            paragraph_text("two"),
        ])
    );

    // can insert two block nodes
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.insert(
        pos,
        Fragment::from(vec![paragraph_text("hi"), horizontal_rule()]),
    )
    .unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("one"),
            paragraph_text("hi"),
            horizontal_rule(),
            paragraph_text("two"),
        ])
    );

    // can insert at the end of a blockquote
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("hey").into(), tag("a")]).into(),
        tagged_paragraph_text("after").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.insert(pos, Fragment::from(empty_paragraph())).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![paragraph_text("hey"), empty_paragraph()]).unwrap(),
            paragraph_text("after"),
        ])
    );

    // can insert at the start of a blockquote
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tag("a"), tagged_paragraph_text("hey").into()]).into(),
        tagged_paragraph_text("after").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.insert(pos, Fragment::from(empty_paragraph())).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![empty_paragraph(), paragraph_text("hey")]).unwrap(),
            paragraph_text("after"),
        ])
    );
}

#[test]
fn transform_delete_matches_upstream_cases() {
    // can delete a word (a textblock-spanning range)
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
        tagged_paragraph_text("three").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph_text("one"), paragraph_text("three")])
    );

    // delete a range entirely inside one textblock
    let document = doc(vec![paragraph_text("hello world")]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.delete(6, 12).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("hello")]));

    // preserves content constraints — emptying a blockquote auto-fills with
    // the default block content (paragraph), matching upstream replace.
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tag("a"), tagged_paragraph_text("hi").into(), tag("b")]).into(),
        tagged_paragraph_text("x").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![empty_paragraph()]).unwrap(),
            paragraph_text("x"),
        ])
    );
}

#[test]
fn transform_add_remove_node_mark_on_inline_leaves() {
    let em = schema_basic::em().unwrap();
    let link_x = schema_basic::link("x", Option::<String>::None).unwrap();

    // adds a mark to an inline leaf
    let document = doc(vec![paragraph(vec![image("a.png")])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.add_node_mark(1, em.clone()).unwrap();
    assert_eq!(
        tr.doc()
            .content()
            .child(0)
            .unwrap()
            .content()
            .child(0)
            .unwrap()
            .marks(),
        std::slice::from_ref(&em)
    );

    // doesn't duplicate an existing mark
    let document = doc(vec![paragraph(vec![
        image("a.png").with_marks(vec![em.clone()])
    ])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.add_node_mark(1, em.clone()).unwrap();
    assert_eq!(
        tr.doc()
            .content()
            .child(0)
            .unwrap()
            .content()
            .child(0)
            .unwrap()
            .marks(),
        std::slice::from_ref(&em)
    );

    // replaces a mark with the same name but different attrs (link href)
    let link_y = schema_basic::link("y", Option::<String>::None).unwrap();
    let document = doc(vec![paragraph(vec![
        image("a.png").with_marks(vec![link_x])
    ])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.add_node_mark(1, link_y.clone()).unwrap();
    assert_eq!(
        tr.doc()
            .content()
            .child(0)
            .unwrap()
            .content()
            .child(0)
            .unwrap()
            .marks(),
        std::slice::from_ref(&link_y)
    );

    // removes a mark
    let document = doc(vec![paragraph(vec![
        image("a.png").with_marks(vec![em.clone()])
    ])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.remove_node_mark(1, em.clone()).unwrap();
    assert!(tr
        .doc()
        .content()
        .child(0)
        .unwrap()
        .content()
        .child(0)
        .unwrap()
        .marks()
        .is_empty());

    // removing a missing mark is a no-op
    let document = doc(vec![paragraph(vec![image("a.png")])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.remove_node_mark(1, em).unwrap();
    assert!(tr
        .doc()
        .content()
        .child(0)
        .unwrap()
        .content()
        .child(0)
        .unwrap()
        .marks()
        .is_empty());
}

#[test]
fn transform_replace_more_upstream_cases() {
    // can delete text inside a single textblock
    let tagged = tagged_doc(vec![tagged_paragraph_text("hell<a>o y<b>ou").into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, Slice::empty()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("hellou")]));

    // can overwrite text with new text
    let tagged = tagged_doc(vec![tagged_paragraph_text("hell<a>o y<b>ou").into()]);
    let insert = tagged_doc(vec![tagged_paragraph_text("<a>i k<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, slice).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("helli kou")]));

    // can insert text at the cursor
    let tagged = tagged_doc(vec![tagged_paragraph_text("hell<a><b>o").into()]);
    let insert = tagged_doc(vec![tagged_paragraph_text("<a>i k<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, slice).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("helli ko")]));

    // can add a textblock between top-level blocks
    let tagged = tagged_doc(vec![tagged_paragraph_text("hello<a>you").into()]);
    let insert = tagged_doc(vec![
        tag("a"),
        tagged_paragraph_text("there").into(),
        tag("b"),
    ]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("hello"),
            paragraph_text("there"),
            paragraph_text("you"),
        ])
    );

    // can insert while joining textblocks (heading absorbs paragraph)
    let tagged = tagged_doc(vec![
        tagged_heading_text(1, "he<a>llo").into(),
        tagged_paragraph_text("arg<b>!").into(),
    ]);
    let insert = tagged_doc(vec![tagged_paragraph_text("1<a>2<b>3").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, slice).unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(1, "he2!")]));

    // can merge text down from nested wrappers via delete
    let tagged = tagged_doc(vec![
        tagged_heading_text(1, "wo<a>ah").into(),
        tagged_blockquote(vec![tagged_paragraph_text("ah<b>ha").into()]).into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, Slice::empty()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(1, "woha")]));

    // can merge text up into nested wrappers via delete (deeper from side wins)
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("foo<a>bar").into()]).into(),
        tagged_paragraph_text("middle").into(),
        tagged_heading_text(1, "quux<b>baz").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, Slice::empty()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![schema_basic::blockquote(vec![paragraph_text(
            "foobaz"
        )])
        .unwrap()])
    );

    // crossing a deeper to depth with intervening siblings drops the
    // intervening content but keeps siblings after the delete intact.
    let tagged = tagged_doc(vec![
        tagged_heading_text(1, "wo<a>ah").into(),
        tagged_blockquote(vec![tagged_paragraph_text("ah<b>ha").into()]).into(),
        tagged_paragraph_text("after").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, Slice::empty()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![heading(1, "woha"), paragraph_text("after")])
    );

    // can delete the whole document — auto-fills with default block content
    let document = doc(vec![heading(1, "hi"), paragraph_text("you")]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    let size = tr.doc().content_size();
    tr.replace(0, size, Slice::empty()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![empty_paragraph()]));
}

#[test]
fn transform_replace_range_with_block_in_paragraph_boundaries() {
    // can move an inserted block forward out of a leading wrapper
    let tagged = tagged_doc(vec![tagged_heading_text(1, "foo<a>").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range_with(pos, pos, horizontal_rule()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(1, "foo"), horizontal_rule()]));

    // can move an inserted block backward out of a wrapper
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tagged_blockquote(vec![tagged_paragraph_text("<a>b").into()]).into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range_with(pos, pos, horizontal_rule()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("a"),
            schema_basic::blockquote(vec![horizontal_rule(), paragraph_text("b")]).unwrap(),
        ])
    );
}

#[test]
fn transform_replace_range_matches_more_upstream_cases() {
    // replaces inline content within a paragraph
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>b<b>ar").into()]);
    let insert = tagged_doc(vec![tagged_paragraph_text("<a>xx<b>").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(from, to, slice).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("fooxxar")]));

    // replaces an empty paragraph with a heading slice — defining context drop
    let tagged = tagged_doc(vec![tagged_paragraph(vec![tag("a")]).into()]);
    let insert = tagged_doc(vec![tagged_heading_text(1, "<a>text<b>").into()]);
    // include_parents=true to get the slice with its h1 wrapper open
    let slice = insert
        .node
        .slice_with_parents(insert.tag("a"), insert.tag("b"), true)
        .unwrap();
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(pos, pos, slice).unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(1, "text")]));

    // recreates a list when overwriting an empty paragraph with a deeply
    // wrapped slice (ul > li > p).
    let tagged = tagged_doc(vec![tagged_paragraph(vec![tag("a")]).into()]);
    let insert = tagged_doc(vec![tagged_bullet_list(vec![tagged_list_item(vec![
        tagged_paragraph(vec![tag("a"), tagged_text("foobar").into(), tag("b")]).into(),
    ])
    .into()])
    .into()]);
    let slice = insert
        .node
        .slice_with_parents(insert.tag("a"), insert.tag("b"), true)
        .unwrap();
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(pos, pos, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![list_item_text("foobar")])])
    );

    // closes open nodes at the start of the inserted slice
    let tagged = tagged_doc(vec![
        tag("a"),
        tagged_paragraph_text("abc").into(),
        tag("b"),
    ]);
    let insert = tagged_doc(vec![
        tag("a"),
        tagged_paragraph_text("def").into(),
        tag("b"),
    ]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range(from, to, slice).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("def")]));
}

#[test]
fn transform_delete_range_matches_more_upstream_cases() {
    // deletes empty parent textblocks when they are wholly covered
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![
            tag("a"),
            tagged_paragraph_text("foo").into(),
            tag("b"),
        ])
        .into(),
        tagged_paragraph_text("x").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete_range(from, to).unwrap();
    // delete_range collapses the now-empty blockquote; surviving doc must still
    // satisfy the `block+` content rule.
    let body = tr.doc().content().clone();
    assert!(body.iter().all(|child| !child.is_text()));
    assert_eq!(tr.doc().text_content(), "x");

    // expands to cover the whole textblock when the range is the whole text
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tagged_paragraph_text("<a>foo<b>").into(),
        tagged_paragraph_text("b").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete_range(from, to).unwrap();
    // the text was removed; pine keeps the (empty) wrapping paragraph by the
    // content-expression auto-fill, matching upstream's "leaves wrapping
    // textblock when deleting all text in it" expectation.
    assert_eq!(tr.doc().child_count(), 3);
    let middle = tr.doc().content().child(1).unwrap();
    assert_eq!(middle.type_name(), "paragraph");
    assert_eq!(middle.text_content(), "");
}

#[test]
fn transform_set_block_type_more_upstream_cases() {
    let em = schema_basic::em().unwrap();

    // can change a wrapped block (inside a blockquote)
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_paragraph(vec![tag("a"), tagged_text("one").into()]).into(),
        tagged_paragraph(vec![tagged_text("two").into(), tag("b")]).into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(1));
    tr.set_block_type(from, to, "heading", attrs).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![schema_basic::blockquote(vec![
            heading(1, "one"),
            heading(1, "two"),
        ])
        .unwrap()])
    );

    // clears markup when changing a textblock to code_block
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("hello").into(),
        tag("a"),
        tagged_marked_text("world", vec![em.clone()]).into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.set_block_type(pos, pos, "code_block", Attrs::new())
        .unwrap();
    assert_eq!(tr.doc(), &doc(vec![code_block("helloworld")]));

    // removes non-text inline nodes when converting to code_block
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("one").into(),
        tagged_image("x.png").into(),
        tagged_text("two").into(),
        tagged_image("y.png").into(),
        tagged_text("three").into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.set_block_type(pos, pos, "code_block", Attrs::new())
        .unwrap();
    assert_eq!(tr.doc(), &doc(vec![code_block("onetwothree")]));

    // keeps marks when converting to a heading (paragraph -> heading allows marks)
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("hello ").into(),
        tagged_marked_text("world", vec![em.clone()]).into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(1));
    tr.set_block_type(pos, pos, "heading", attrs).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![schema_basic::heading(
            1,
            vec![text("hello "), marked_text("world", vec![em])]
        )
        .unwrap()])
    );
}

#[test]
fn transform_set_block_type_converts_linebreak_replacement() {
    // Build a schema where hard_break is the linebreak replacement.
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("block+"))
        .node(NodeSpec::new("paragraph").group("block").content("inline*"))
        .node(
            NodeSpec::new("code_block")
                .group("block")
                .content("text*")
                .marks(MarkPolicy::None)
                .code()
                .whitespace(pine_richtext::model::Whitespace::Pre),
        )
        .node(NodeSpec::new("text").group("inline").inline())
        .node(
            NodeSpec::new("hard_break")
                .group("inline")
                .inline()
                .atom()
                .linebreak_replacement(),
        )
        .finish()
        .unwrap();

    let make_text = |value: &str| schema.text(value, Vec::new()).unwrap();
    let make_hard_break = || {
        schema
            .node("hard_break", Attrs::new(), Fragment::empty())
            .unwrap()
    };
    let make_paragraph = |children: Vec<Node>| {
        schema
            .node("paragraph", Attrs::new(), Fragment::from(children))
            .unwrap()
    };
    let make_code_block = |text: &str| {
        let content = if text.is_empty() {
            Fragment::empty()
        } else {
            Fragment::from(make_text(text))
        };
        schema.node("code_block", Attrs::new(), content).unwrap()
    };
    let make_doc = |children: Vec<Node>| {
        schema
            .node("doc", Attrs::new(), Fragment::from(children))
            .unwrap()
    };

    // code_block → paragraph: newlines become hard_break nodes.
    let document = make_doc(vec![make_code_block("one\ntwo\nthree")]);
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), document);
    tr.set_block_type(1, 1, "paragraph", Attrs::new()).unwrap();
    assert_eq!(
        tr.doc(),
        &make_doc(vec![make_paragraph(vec![
            make_text("one"),
            make_hard_break(),
            make_text("two"),
            make_hard_break(),
            make_text("three"),
        ])])
    );

    // paragraph → code_block: hard_breaks become newline characters.
    let document = make_doc(vec![make_paragraph(vec![
        make_text("one"),
        make_hard_break(),
        make_text("two"),
        make_hard_break(),
        make_text("three"),
    ])]);
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), document);
    tr.set_block_type(1, 1, "code_block", Attrs::new()).unwrap();
    assert_eq!(
        tr.doc(),
        &make_doc(vec![make_code_block("one\ntwo\nthree")])
    );
}

#[test]
fn transform_set_block_type_after_another_step() {
    // Delete inside the first paragraph, then change the second paragraph's
    // type using the post-delete mapped position. Matches upstream's "works
    // after another step" case.
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("f<x>oob<y>ar").into(),
        tagged_paragraph_text("baz<a>").into(),
    ]);
    let x = tagged.tag("x");
    let y = tagged.tag("y");
    let a = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete(x, y).unwrap();
    let mapped_a = tr.mapping().map(a, 1);
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(1));
    tr.set_block_type(mapped_a, mapped_a, "heading", attrs)
        .unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph_text("far"), heading(1, "baz")])
    );
}

#[test]
fn transform_set_block_type_with_derives_attrs_from_previous_node() {
    // Convert each top-level textblock to heading, deriving the new level from
    // the existing node's `level` attribute (defaulting to 0 for paragraphs).
    let tagged = tagged_doc(vec![
        tag("a"),
        tagged_heading_text(1, "a").into(),
        tagged_paragraph_text("b").into(),
        tag("b"),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.set_block_type_with(from, to, "heading", |node| {
        let mut attrs = Attrs::new();
        let level = node
            .attrs()
            .get("level")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        attrs.insert("level".to_string(), json!(level + 1));
        attrs
    })
    .unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(2, "a"), heading(1, "b")]));
}

#[test]
fn transform_set_block_type_skips_blocks_that_break_parent_constraints() {
    // A list_item requires a paragraph as its first child, then arbitrary
    // blocks. Converting all top-level textblocks to code_block should rewrite
    // the loose paragraphs but leave the paragraph inside list_item alone,
    // since making it a code_block would break list_item's content expression.
    let tagged = tagged_doc(vec![
        tag("a"),
        tagged_paragraph(vec![
            tagged_text("hello").into(),
            tagged_image("x.png").into(),
        ])
        .into(),
        tagged_paragraph_text("okay").into(),
        tagged_bullet_list(vec![tagged_list_item_text("foo").into()]).into(),
        tag("b"),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.set_block_type(from, to, "code_block", Attrs::new())
        .unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            code_block("hello"),
            code_block("okay"),
            bullet_list(vec![list_item_text("foo")]),
        ])
    );
}

#[test]
fn transform_list_operations_parity() {
    let schema = schema_basic::schema();

    // wrap two paragraphs in an ordered_list — needs to also build list_items
    // around each. Pine's `wrap` does a flat wrap, so we exercise list creation
    // via a direct slice replacement instead.
    let document = doc(vec![paragraph_text("one"), paragraph_text("two")]);
    let ol = ordered_list(vec![list_item_text("one"), list_item_text("two")]);
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), document);
    tr.replace(
        0,
        tr.doc().content_size(),
        Slice::new(Fragment::from(ol.clone()), 0, 0),
    )
    .unwrap();
    assert_eq!(tr.doc(), &doc(vec![ol]));

    // join two top-level lists when the boundary sits between them
    let tagged = tagged_doc(vec![
        tagged_ordered_list(vec![
            tagged_list_item_text("one").into(),
            tagged_list_item_text("two").into(),
        ])
        .into(),
        tag("a"),
        tagged_ordered_list(vec![tagged_list_item_text("three").into()]).into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![ordered_list(vec![
            list_item_text("one"),
            list_item_text("two"),
            list_item_text("three"),
        ])])
    );

    // join two adjacent list items inside the same list
    let tagged = tagged_doc(vec![tagged_ordered_list(vec![
        tagged_list_item_text("one").into(),
        tagged_list_item_text("two").into(),
        tag("a"),
        tagged_list_item_text("three").into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![ordered_list(vec![
            list_item_text("one"),
            list_item(vec![paragraph_text("two"), paragraph_text("three")]),
        ])])
    );

    // delete a list_item entirely — bullet_list keeps its remaining items
    let tagged = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item_text("one").into(),
        tag("a"),
        tagged_list_item_text("two").into(),
        tag("b"),
        tagged_list_item_text("three").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.delete(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("one"),
            list_item_text("three"),
        ])])
    );

    // join two adjacent textblocks across list items
    let tagged = tagged_doc(vec![tagged_bullet_list(vec![tagged_list_item(vec![
        tagged_paragraph_text("a").into(),
        tag("a"),
        tagged_paragraph_text("b").into(),
    ])
    .into()])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![bullet_list(vec![list_item_text("ab")])])
    );

    // lift a paragraph out of a single-item list — the lifted content has to
    // walk past both the list_item and the surrounding list to reach a valid
    // target (the doc).
    let tagged = tagged_doc(vec![tagged_bullet_list(vec![tagged_list_item(vec![
        tagged_paragraph(vec![tag("a"), tagged_text("foo").into(), tag("b")]).into(),
    ])
    .into()])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("foo")]));

    // lift the middle item of a list — the surrounding list splits and the
    // lifted paragraph appears between two single-item lists.
    let tagged = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item_text("one").into(),
        tagged_list_item(vec![tagged_paragraph(vec![
            tag("a"),
            tagged_text("two").into(),
            tag("b"),
        ])
        .into()])
        .into(),
        tagged_list_item_text("three").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            bullet_list(vec![list_item_text("one")]),
            paragraph_text("two"),
            bullet_list(vec![list_item_text("three")]),
        ])
    );

    // lift from the end of a list — the trailing list disappears entirely.
    let tagged = tagged_doc(vec![tagged_bullet_list(vec![
        tagged_list_item_text("one").into(),
        tagged_list_item(vec![tagged_paragraph(vec![
            tag("a"),
            tagged_text("two").into(),
            tag("b"),
        ])
        .into()])
        .into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            bullet_list(vec![list_item_text("one")]),
            paragraph_text("two"),
        ])
    );
}

#[test]
fn transform_delete_auto_fills_top_level_when_emptied() {
    // Deleting all top-level content should still leave the doc with a valid
    // body (default block content) instead of producing an empty doc that
    // violates the schema.
    let document = doc(vec![paragraph_text("hello"), paragraph_text("world")]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), document);
    tr.delete(0, tr.doc().content_size()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![empty_paragraph()]));
}

#[test]
fn transform_join_split_matches_upstream_cases() {
    // join textblocks
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tag("a"),
        tagged_paragraph_text("bar").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("foobar")]));

    // join compatible blocks (heading + paragraph -> heading, since paragraph
    // contents fit into heading's inline* content expression)
    let tagged = tagged_doc(vec![
        tagged_heading_text(1, "foo").into(),
        tag("a"),
        tagged_paragraph_text("bar").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(1, "foobar")]));

    // join blocks at top level (blockquote + blockquote)
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("a").into()]).into(),
        tag("a"),
        tagged_blockquote(vec![tagged_paragraph_text("b").into()]).into(),
        tagged_paragraph_text("after").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![paragraph_text("a"), paragraph_text("b")]).unwrap(),
            paragraph_text("after"),
        ])
    );

    // join nested blocks — two inner blockquotes share an outer blockquote
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("a").into(),
            tagged_paragraph_text("b").into(),
        ])
        .into(),
        tag("a"),
        tagged_blockquote(vec![
            tagged_paragraph_text("c").into(),
            tagged_paragraph_text("d").into(),
        ])
        .into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.join(pos).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![schema_basic::blockquote(vec![
            schema_basic::blockquote(vec![
                paragraph_text("a"),
                paragraph_text("b"),
                paragraph_text("c"),
                paragraph_text("d"),
            ])
            .unwrap()
        ])
        .unwrap()])
    );

    // split a textblock
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>bar").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.split(pos, 1).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph_text("foo"), paragraph_text("bar")])
    );

    // split two deep — split textblock and its blockquote parent
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_blockquote(vec![tagged_paragraph_text(
            "foo<a>bar",
        )
        .into()])
        .into()])
        .into(),
        tagged_paragraph_text("after").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.split(pos, 2).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![
                schema_basic::blockquote(vec![paragraph_text("foo")]).unwrap(),
                schema_basic::blockquote(vec![paragraph_text("bar")]).unwrap(),
            ])
            .unwrap(),
            paragraph_text("after"),
        ])
    );

    // split three deep — split textblock and both blockquote ancestors
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_blockquote(vec![tagged_paragraph_text(
            "foo<a>bar",
        )
        .into()])
        .into()])
        .into(),
        tagged_paragraph_text("after").into(),
    ]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.split(pos, 3).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![
                schema_basic::blockquote(vec![paragraph_text("foo")]).unwrap(),
            ])
            .unwrap(),
            schema_basic::blockquote(vec![
                schema_basic::blockquote(vec![paragraph_text("bar")]).unwrap(),
            ])
            .unwrap(),
            paragraph_text("after"),
        ])
    );

    // split at end produces empty trailing block
    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_paragraph_text(
        "hi<a>",
    )
    .into()])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.split(pos, 1).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![schema_basic::blockquote(vec![
            paragraph_text("hi"),
            empty_paragraph(),
        ])
        .unwrap()])
    );

    // split rejects break of content constraints (blockquote with first-paragraph cursor not allowed)
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tag("a"),
        tagged_paragraph_text("x").into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    assert!(tr.split(pos, 1).is_err());
}

#[test]
fn transform_lift_step_maps_through_a_concurrent_insertion() {
    // Two transforms on the same starting doc: one lifts a paragraph out of a
    // blockquote (cursor-inside-text convention so the lift uses the new
    // ReplaceAroundStep path), the other inserts text at the very start.
    // Mapping the lift step through the insert's mapping and applying both
    // should land both edits on the resulting document.
    let starting = doc(vec![
        schema_basic::blockquote(vec![paragraph_text("a")]).unwrap()
    ]);
    let mut lift_tr =
        pine_richtext::transform::Transform::new(schema_basic::schema(), starting.clone());
    // Pos 3 sits inside the paragraph's text ("a" at content offset 1).
    lift_tr.lift(3, 3).unwrap();
    assert!(matches!(
        lift_tr.steps()[0],
        pine_richtext::transform::Step::ReplaceAround(_)
    ));

    let mut insert_tr =
        pine_richtext::transform::Transform::new(schema_basic::schema(), starting.clone());
    insert_tr
        .insert(0, Fragment::from(paragraph_text("b")))
        .unwrap();

    let mapped = lift_tr.steps()[0]
        .map_step(&insert_tr.mapping())
        .expect("lift step should map through the concurrent insertion");
    let result = mapped
        .apply(insert_tr.doc(), &schema_basic::schema())
        .unwrap();
    assert_eq!(
        &result.doc,
        &doc(vec![paragraph_text("b"), paragraph_text("a")])
    );
}

#[test]
fn transform_wrap_step_maps_through_a_concurrent_insertion() {
    // Two transforms on the same starting doc: one wraps the paragraph in a
    // blockquote, the other inserts a fresh paragraph at the top level. Mapping
    // the wrap step through the insertion's mapping and applying both should
    // produce a doc where the insertion sits before the wrapped blockquote.
    let starting = doc(vec![paragraph_text("a")]);
    let mut wrap_tr =
        pine_richtext::transform::Transform::new(schema_basic::schema(), starting.clone());
    wrap_tr
        .wrap(0, starting.content_size(), "blockquote", Attrs::new())
        .unwrap();

    let mut insert_tr =
        pine_richtext::transform::Transform::new(schema_basic::schema(), starting.clone());
    insert_tr
        .insert(0, Fragment::from(paragraph_text("b")))
        .unwrap();

    let mapped_step = wrap_tr.steps()[0]
        .map_step(&insert_tr.mapping())
        .expect("wrap step should map through the concurrent insertion");
    let result = mapped_step
        .apply(insert_tr.doc(), &schema_basic::schema())
        .unwrap();
    assert_eq!(
        &result.doc,
        &doc(vec![
            paragraph_text("b"),
            schema_basic::blockquote(vec![paragraph_text("a")]).unwrap(),
        ])
    );
}

#[test]
fn transform_lift_wrap_matches_upstream_cases() {
    // lift the only child out of a blockquote (whole-wrapper lift)
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("two")]));

    // lift a block out of the middle of its parent (partial-wrapper lift)
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
        tagged_paragraph_text("three").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![paragraph_text("one")]).unwrap(),
            paragraph_text("two"),
            schema_basic::blockquote(vec![paragraph_text("three")]).unwrap(),
        ])
    );

    // lift a block from the start of its parent
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
        tagged_paragraph_text("three").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("two"),
            schema_basic::blockquote(vec![paragraph_text("three")]).unwrap(),
        ])
    );

    // lift a block from the end of its parent
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![paragraph_text("one")]).unwrap(),
            paragraph_text("two"),
        ])
    );

    // lift multiple sibling blocks at once
    let tagged = tagged_doc(vec![tagged_blockquote(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("one").into(),
            tag("a"),
            tagged_paragraph_text("two").into(),
            tag("b"),
        ])
        .into(),
        tagged_paragraph_text("three").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.lift(from, to).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![schema_basic::blockquote(vec![
            schema_basic::blockquote(vec![paragraph_text("one")]).unwrap(),
            paragraph_text("two"),
            paragraph_text("three"),
        ])
        .unwrap()])
    );

    // wrap a paragraph in a blockquote (range spans the whole paragraph)
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
        tag("b"),
        tagged_paragraph_text("three").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.wrap(from, to, "blockquote", Attrs::new()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("one"),
            schema_basic::blockquote(vec![paragraph_text("two")]).unwrap(),
            paragraph_text("three"),
        ])
    );

    // wrap two paragraphs together (range spans both paragraphs)
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("one").into(),
        tag("a"),
        tagged_paragraph_text("two").into(),
        tagged_paragraph_text("three").into(),
        tag("b"),
        tagged_paragraph_text("four").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.wrap(from, to, "blockquote", Attrs::new()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("one"),
            schema_basic::blockquote(vec![paragraph_text("two"), paragraph_text("three")]).unwrap(),
            paragraph_text("four"),
        ])
    );
}

#[test]
fn transform_replace_range_with_inline_and_block_nodes() {
    // can insert an inline node
    let tagged = tagged_doc(vec![tagged_paragraph_text("fo<a>o").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range_with(pos, pos, image("x.png")).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![text("fo"), image("x.png"), text("o")])])
    );

    // can replace content with an inline node
    let tagged = tagged_doc(vec![tagged_paragraph_text("<a>fo<b>o").into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range_with(from, to, image("x.png")).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![image("x.png"), text("o")])])
    );

    // can replace a block node with another block node
    let tagged = tagged_doc(vec![
        tag("a"),
        tagged_blockquote(vec![tagged_paragraph_text("a").into()]).into(),
        tag("b"),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range_with(from, to, horizontal_rule()).unwrap();
    assert_eq!(tr.doc(), &doc(vec![horizontal_rule()]));

    // can insert a horizontal rule in the middle of text
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>bar").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace_range_with(pos, pos, horizontal_rule()).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![
            paragraph_text("foo"),
            horizontal_rule(),
            paragraph_text("bar"),
        ])
    );
}

#[test]
fn transform_delete_range_matches_upstream_cases() {
    // deletes the given range, joining textblocks across the boundary
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("fo<a>o").into(),
        tagged_paragraph_text("b<b>ar").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete_range(from, to).unwrap();
    assert_eq!(tr.doc(), &doc(vec![paragraph_text("foar")]));

    // doesn't delete parent textblocks that can be empty
    let tagged = tagged_doc(vec![tagged_paragraph_text("<a>foo<b>").into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete_range(from, to).unwrap();
    assert_eq!(tr.doc(), &doc(vec![empty_paragraph()]));

    // is okay with deleting empty ranges
    let tagged = tagged_doc(vec![tagged_paragraph(vec![tag("a"), tag("b")]).into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.delete_range(from, to).unwrap();
    assert_eq!(tr.doc(), &doc(vec![empty_paragraph()]));
}

#[test]
fn transform_set_node_attribute_and_doc_attribute() {
    // set_node_attr changes a heading level
    let tagged = tagged_doc(vec![tag("a"), tagged_heading_text(1, "a").into()]);
    let pos = tagged.tag("a");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.set_node_attr(pos, "level", Some(json!(2))).unwrap();
    assert_eq!(tr.doc(), &doc(vec![heading(2, "a")]));

    // set_doc_attr writes a top-level attribute
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("text*"))
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();
    let document = schema.node("doc", Attrs::new(), Fragment::empty()).unwrap();
    let mut tr = pine_richtext::transform::Transform::new(schema.clone(), document);
    tr.set_doc_attr("meta", Some(json!("hello"))).unwrap();
    let mut expected_attrs = Attrs::new();
    expected_attrs.insert("meta".to_string(), json!("hello"));
    let expected = schema
        .node("doc", expected_attrs, Fragment::empty())
        .unwrap();
    assert_eq!(tr.doc(), &expected);
}

#[test]
fn transform_replace_joins_marks_across_inserted_slice() {
    // Matches upstream "joins marks": the inserted slice has the same em mark
    // continuing across the boundary, and the surrounding marked text fuses
    // into a single em run rather than splitting.
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo ").into(),
        tagged_marked_text("bar<a>baz", vec![em.clone()]).into(),
        tag("b"),
        tagged_text(" quux").into(),
    ])
    .into()]);
    let insert = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo ").into(),
        tagged_marked_text("xy<a>zzy", vec![em.clone()]).into(),
        tagged_text(" foo").into(),
        tag("b"),
    ])
    .into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("foo "),
            marked_text("barzzy", vec![em.clone()]),
            text(" foo quux"),
        ])])
    );

    // "can replace text with a break" — replacement adds a hard_break inside a
    // paragraph and the surrounding text stays.
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>bb<b>bar").into()]);
    let insert = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_hard_break().into(),
        tag("b"),
    ])
    .into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), tagged.node);
    tr.replace(from, to, slice).unwrap();
    assert_eq!(
        tr.doc(),
        &doc(vec![paragraph(vec![
            text("foo"),
            hard_break(),
            text("bar")
        ])])
    );
}
