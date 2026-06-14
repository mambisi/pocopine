//! Parity tests for `prosemirror-model/test/*.ts`.
//!
//! Split out of the previous monolithic `prosemirror_parity.rs` so the
//! model-domain tests live next to upstream's directory layout. Mirrors
//! `prosemirror-model/test/test-content.ts`, `test-mark.ts`, `test-node.ts`,
//! `test-replace.ts`, `test-resolve.ts`, `test-slice.ts`, and (the model
//! parts of) `test-diff.ts`.

use serde_json::json;

mod support;

use pine_richtext::model::{
    Attrs, ContentExpr, Fragment, Mark, MarkSpec, Node, NodeSpec, Schema, Slice, find_diff_end,
    find_diff_start,
};
use pine_richtext::schema_basic;

use support::*;

#[test]
fn model_text_between_keeps_empty_block_separators() {
    let doc = doc(vec![
        paragraph_text("one"),
        empty_paragraph(),
        paragraph_text("two"),
    ]);

    assert_eq!(
        doc.text_between(0, doc.content_size(), "\n").unwrap(),
        "one\n\ntwo"
    );
}

#[test]
fn model_mark_addition_replaces_same_type_with_new_attrs() {
    let old_link = schema_basic::link("https://old.example", Option::<String>::None).unwrap();
    let new_link = schema_basic::link("https://new.example", Option::<String>::None).unwrap();
    let doc = doc(vec![paragraph(vec![marked_text("link", vec![old_link])])]);
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), doc);

    tr.add_mark(1, 5, new_link.clone()).unwrap();

    let text = tr
        .doc()
        .content()
        .child(0)
        .unwrap()
        .content()
        .child(0)
        .unwrap();
    assert_eq!(text.marks(), std::slice::from_ref(&new_link));
}

#[test]
fn model_mark_set_helpers_match_upstream_cases() {
    let schema = schema_basic::schema();
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let code = schema_basic::code().unwrap();
    let link_foo = schema_basic::link("http://foo", Option::<String>::None).unwrap();
    let link_bar = schema_basic::link("http://bar", Option::<String>::None).unwrap();
    let link_with_title = schema_basic::link("http://foo", Some("title")).unwrap();

    assert!(Mark::same_set(&[], &[]));
    assert!(Mark::same_set(
        &[em.clone(), strong.clone()],
        &[em.clone(), strong.clone()]
    ));
    assert!(!Mark::same_set(
        &[em.clone(), strong.clone()],
        &[em.clone(), code.clone()]
    ));
    assert!(!Mark::same_set(
        &[link_foo.clone(), code.clone()],
        &[link_bar.clone(), code.clone()]
    ));

    assert!(Mark::same_set(
        &em.add_to_set(&schema, &[]).unwrap(),
        std::slice::from_ref(&em)
    ));
    assert!(Mark::same_set(
        &strong
            .add_to_set(&schema, std::slice::from_ref(&em))
            .unwrap(),
        &[em.clone(), strong.clone()]
    ));
    assert!(Mark::same_set(
        &strong
            .add_to_set(&schema, &[link_foo.clone(), em.clone()])
            .unwrap(),
        &[link_foo.clone(), em.clone(), strong]
    ));
    assert!(Mark::same_set(
        &link_bar
            .add_to_set(&schema, &[link_foo.clone(), em.clone()])
            .unwrap(),
        &[link_bar, em.clone()]
    ));

    assert!(Mark::same_set(&em.remove_from_set(&[]), &[]));
    assert!(Mark::same_set(
        &link_foo.remove_from_set(std::slice::from_ref(&link_foo)),
        &[]
    ));
    assert!(Mark::same_set(
        &link_with_title.remove_from_set(std::slice::from_ref(&link_foo)),
        std::slice::from_ref(&link_foo)
    ));
}

#[test]
fn model_diff_start_notices_mark_changes_at_mark_boundary() {
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let left = paragraph(vec![text("a"), marked_text("b", vec![em])]);
    let right = paragraph(vec![text("a"), marked_text("b", vec![strong])]);

    assert_eq!(
        find_diff_start(&Fragment::from(left), &Fragment::from(right), 0),
        Some(2)
    );
}

#[test]
fn model_diff_end_notices_mark_and_attribute_changes() {
    let left = doc(vec![heading(1, "Title")]);
    let right = doc(vec![heading(2, "Title")]);

    // PM's findDiffEnd returns the position AFTER the diverging node (= the
    // start of the matching suffix, going from the end). For two heading
    // nodes with different attrs the whole node is the diff, so the diff
    // ends at the position right after the heading — which for the doc
    // `doc(heading(1, "Title"))` is heading.node_size = 7.
    let size = left.content_size();
    assert_eq!(
        find_diff_end(left.content(), right.content(), 0, 0),
        Some((size, size))
    );
}

#[test]
fn model_content_expression_supports_alternatives_and_quantifiers() {
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("(paragraph | heading)+"))
        .node(NodeSpec::new("paragraph").content("text*"))
        .node(NodeSpec::new("heading").content("text*"))
        .node(NodeSpec::new("text").inline())
        .mark(MarkSpec::new("strong"))
        .finish()
        .unwrap();

    let paragraph = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("body", Vec::new()).unwrap()),
        )
        .unwrap();
    let heading = schema
        .node(
            "heading",
            Attrs::new(),
            Fragment::from(schema.text("title", Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![heading, paragraph]),
        )
        .unwrap();

    schema.check_node(&doc).unwrap();
    assert_eq!(doc.text_content(), "titlebody");
}

#[test]
fn model_content_expression_supports_nested_repeats_and_counts() {
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("(heading paragraph+)+"))
        .node(NodeSpec::new("heading").content("text*"))
        .node(NodeSpec::new("paragraph").content("text*"))
        .node(NodeSpec::new("horizontal_rule"))
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();
    let heading = || heading_with_schema(&schema, "h");
    let paragraph = || paragraph_with_schema(&schema, "p");

    let valid = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![
                heading(),
                paragraph(),
                heading(),
                paragraph(),
                paragraph(),
            ]),
        )
        .unwrap();
    schema.check_node(&valid).unwrap();

    let invalid = schema.node(
        "doc",
        Attrs::new(),
        Fragment::from(vec![
            heading(),
            paragraph(),
            heading(),
            paragraph(),
            paragraph(),
            schema
                .node("horizontal_rule", Attrs::new(), Fragment::empty())
                .unwrap(),
        ]),
    );
    assert!(invalid.is_err());

    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("hard_break{2, 4} image?"))
        .node(NodeSpec::new("hard_break"))
        .node(NodeSpec::new("image"))
        .finish()
        .unwrap();
    let hard_break = || {
        schema
            .node("hard_break", Attrs::new(), Fragment::empty())
            .unwrap()
    };
    let image = || {
        schema
            .node("image", Attrs::new(), Fragment::empty())
            .unwrap()
    };

    schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![hard_break(), hard_break(), image()]),
        )
        .unwrap();
    schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![hard_break(), hard_break(), hard_break(), hard_break()]),
        )
        .unwrap();
    assert!(
        schema
            .node("doc", Attrs::new(), Fragment::from(hard_break()))
            .is_err()
    );
    assert!(
        schema
            .node(
                "doc",
                Attrs::new(),
                Fragment::from(vec![
                    hard_break(),
                    hard_break(),
                    hard_break(),
                    hard_break(),
                    hard_break()
                ]),
            )
            .is_err()
    );
}

#[test]
fn model_content_match_fills_before_fragments() {
    let schema = content_match_schema();

    assert_fill_before(
        &schema,
        "paragraph horizontal_rule paragraph",
        &["paragraph", "horizontal_rule"],
        &["paragraph"],
        true,
        Some(&[]),
    );
    assert_fill_before(
        &schema,
        "paragraph horizontal_rule paragraph",
        &["paragraph"],
        &["paragraph"],
        true,
        Some(&["horizontal_rule"]),
    );
    assert_fill_before(
        &schema,
        "hard_break+",
        &[],
        &[],
        true,
        Some(&["hard_break"]),
    );
    assert_fill_before(&schema, "hard_break+", &[], &["image"], true, None);
    assert_fill_before(
        &schema,
        "heading paragraph? horizontal_rule",
        &["heading"],
        &[],
        true,
        Some(&["horizontal_rule"]),
    );
    assert_fill_before(
        &schema,
        "hard_break{3}",
        &["hard_break"],
        &["hard_break"],
        true,
        Some(&["hard_break"]),
    );
}

#[test]
fn model_content_match_fills_across_two_bounds() {
    let schema = content_match_schema();
    let expr = ContentExpr::parse("code_block{3} paragraph{3}").unwrap();
    let before = fragment_for_types(&schema, &["code_block"]);
    let middle = fragment_for_types(&schema, &["paragraph"]);
    let after = Fragment::empty();

    let left = expr
        .match_fragment(&schema, &before)
        .unwrap()
        .fill_before(&middle, false)
        .unwrap();
    assert_fragment_types(&left, &["code_block", "code_block"]);

    let content = before.clone().append(left).append(middle);
    let right = expr
        .match_fragment(&schema, &content)
        .unwrap()
        .fill_before(&after, true)
        .unwrap();
    assert_fragment_types(&right, &["paragraph", "paragraph"]);

    let expr = ContentExpr::parse("paragraph{2}").unwrap();
    let before = fragment_for_types(&schema, &["paragraph"]);
    let middle = fragment_for_types(&schema, &["paragraph"]);
    let after = fragment_for_types(&schema, &["paragraph"]);
    let left = expr
        .match_fragment(&schema, &before)
        .unwrap()
        .fill_before(&middle, false)
        .unwrap();
    assert!(
        expr.match_fragment(&schema, &before.append(left).append(middle))
            .unwrap()
            .fill_before(&after, true)
            .is_none()
    );
}

#[test]
fn model_resolved_positions_reflect_document_structure() {
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("ab").into(),
        tag("between"),
        tagged_blockquote(vec![
            tagged_paragraph(vec![
                tagged_marked_text("cd", vec![em]).into(),
                tagged_text("ef").into(),
            ])
            .into(),
        ])
        .into(),
    ]);
    assert_eq!(tagged.tag("between"), 4);
    let doc = tagged.node;

    let expected = [
        (0, 0, 0, None, Some("ab")),
        (1, 1, 0, None, Some("ab")),
        (2, 1, 1, Some("a"), Some("b")),
        (3, 1, 2, Some("ab"), None),
        (4, 0, 4, Some("ab"), Some("cdef")),
        (5, 1, 0, None, Some("cdef")),
        (6, 2, 0, None, Some("cd")),
        (7, 2, 1, Some("c"), Some("d")),
        (8, 2, 2, Some("cd"), Some("ef")),
        (9, 2, 3, Some("e"), Some("f")),
        (10, 2, 4, Some("ef"), None),
        (11, 1, 6, Some("cdef"), None),
        (12, 0, 12, Some("cdef"), None),
    ];

    for (pos, depth, parent_offset, before, after) in expected {
        let resolved = doc.resolve(pos).unwrap();
        assert_eq!(resolved.depth(), depth, "depth at {pos}");
        assert_eq!(
            resolved.parent_offset(),
            parent_offset,
            "parent offset at {pos}"
        );
        assert_eq!(
            text_content(resolved.node_before()).as_deref(),
            before,
            "before at {pos}"
        );
        assert_eq!(
            text_content(resolved.node_after()).as_deref(),
            after,
            "after at {pos}"
        );
    }

    let resolved = doc.resolve(7).unwrap();
    assert_eq!(resolved.start(2), Some(6));
    assert_eq!(resolved.end(2), Some(10));
    assert_eq!(resolved.before(2), Some(5));
    assert_eq!(resolved.after(2), Some(11));
    assert_eq!(resolved.pos_at_index(1, 2), Some(8));
    assert!(resolved.same_parent(&doc.resolve(9).unwrap()));
}

#[test]
fn model_resolved_positions_report_active_marks() {
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let schema = schema_basic::schema();

    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![tagged_marked_text("fo<a>o", vec![em.clone()]).into()]).into(),
    ]);
    assert!(em.is_in_set(&tagged.node.resolve(tagged.tag("a")).unwrap().marks(&schema)));
    assert!(!strong.is_in_set(&tagged.node.resolve(tagged.tag("a")).unwrap().marks(&schema)));

    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_marked_text("hi", vec![em.clone()]).into(),
            tagged_text("<a> there").into(),
        ])
        .into(),
    ]);
    assert!(em.is_in_set(&tagged.node.resolve(tagged.tag("a")).unwrap().marks(&schema)));

    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("one <a>").into(),
            tagged_marked_text("two", vec![em.clone()]).into(),
        ])
        .into(),
    ]);
    assert!(!em.is_in_set(&tagged.node.resolve(tagged.tag("a")).unwrap().marks(&schema)));

    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![tagged_marked_text("<a>one", vec![em.clone()]).into()]).into(),
    ]);
    assert!(em.is_in_set(&tagged.node.resolve(tagged.tag("a")).unwrap().marks(&schema)));

    let link = schema_basic::link("https://example.test", Option::<String>::None).unwrap();
    let other_link = schema_basic::link("https://other.test", Option::<String>::None).unwrap();
    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![tagged_marked_text("li<a>nk", vec![link]).into()]).into(),
    ]);
    assert!(!other_link.is_in_set(&tagged.node.resolve(tagged.tag("a")).unwrap().marks(&schema)));
}

#[test]
fn model_resolved_positions_handle_non_inclusive_marks() {
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("paragraph+"))
        .node(NodeSpec::new("paragraph").content("text*"))
        .node(NodeSpec::new("text").inline())
        .mark(MarkSpec::new("remark").excludes("").inclusive(false))
        .mark(MarkSpec::new("strong").excludes("em-group"))
        .mark(MarkSpec::new("em").group("em-group"))
        .finish()
        .unwrap();
    let remark1 = schema
        .mark("remark", [("id".to_string(), json!(1))].into())
        .unwrap();
    let remark2 = schema
        .mark("remark", [("id".to_string(), json!(2))].into())
        .unwrap();
    let strong = schema.mark("strong", Attrs::new()).unwrap();

    let doc = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![
                schema
                    .node(
                        "paragraph",
                        Attrs::new(),
                        Fragment::from(vec![
                            schema
                                .text("one", vec![remark1.clone(), strong.clone()])
                                .unwrap(),
                            schema.text("two", Vec::new()).unwrap(),
                        ]),
                    )
                    .unwrap(),
                schema
                    .node(
                        "paragraph",
                        Attrs::new(),
                        Fragment::from(vec![
                            schema.text("one", Vec::new()).unwrap(),
                            schema.text("two", vec![remark1.clone()]).unwrap(),
                            schema.text("three", vec![remark1.clone()]).unwrap(),
                        ]),
                    )
                    .unwrap(),
                schema
                    .node(
                        "paragraph",
                        Attrs::new(),
                        Fragment::from(vec![
                            schema.text("one", vec![remark2]).unwrap(),
                            schema.text("two", vec![remark1.clone()]).unwrap(),
                        ]),
                    )
                    .unwrap(),
            ]),
        )
        .unwrap();

    assert_eq!(doc.resolve(4).unwrap().marks(&schema), vec![strong.clone()]);
    assert_eq!(
        doc.resolve(3).unwrap().marks(&schema),
        vec![remark1.clone(), strong]
    );
    assert!(doc.resolve(20).unwrap().marks(&schema).is_empty());
    assert_eq!(doc.resolve(15).unwrap().marks(&schema), vec![remark1]);
    assert!(doc.resolve(25).unwrap().marks(&schema).is_empty());
}

#[test]
fn model_slice_preserves_open_depths() {
    let left_open = tagged_doc(vec![tagged_paragraph_text("hello<a> world").into()]);
    let slice = left_open
        .node
        .slice(left_open.tag("a"), left_open.node.content_size())
        .unwrap();
    assert_eq!(slice.content.size(), 8);
    assert_eq!(
        slice.content.text_between(0, slice.content.size(), ""),
        " world"
    );
    assert_eq!(slice.open_start, 1);
    assert_eq!(slice.open_end, 0);

    let right_open = tagged_doc(vec![tagged_paragraph_text("hello<b> world").into()]);
    let slice = right_open.node.slice(0, right_open.tag("b")).unwrap();
    assert_eq!(
        slice.content.text_between(0, slice.content.size(), ""),
        "hello"
    );
    assert_eq!(slice.open_start, 0);
    assert_eq!(slice.open_end, 1);

    let text_only = tagged_doc(vec![tagged_paragraph_text("hell<a>o wo<b>rld").into()]);
    let slice = text_only
        .node
        .slice(text_only.tag("a"), text_only.tag("b"))
        .unwrap();
    assert_eq!(
        slice.content.text_between(0, slice.content.size(), ""),
        "o wo"
    );
    assert_eq!(slice.open_start, 0);
    assert_eq!(slice.open_end, 0);
}

#[test]
fn model_slice_matches_upstream_slice_matrix() {
    let case = tagged_doc(vec![tagged_paragraph_text("hello<b> world").into()]);
    assert_slice(
        &case,
        None,
        Some("b"),
        &doc(vec![paragraph_text("hello")]),
        0,
        1,
    );

    let case = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tagged_paragraph_text("b<b>").into(),
    ]);
    assert_slice(
        &case,
        None,
        Some("b"),
        &doc(vec![paragraph_text("a"), paragraph_text("b")]),
        0,
        1,
    );

    let case = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tag("b"),
        tagged_paragraph_text("b").into(),
    ]);
    assert_slice(
        &case,
        None,
        Some("b"),
        &doc(vec![paragraph_text("a")]),
        0,
        0,
    );

    let case = tagged_doc(vec![tagged_paragraph_text("hello<a> world").into()]);
    assert_slice(
        &case,
        Some("a"),
        None,
        &doc(vec![paragraph_text(" world")]),
        1,
        0,
    );

    let case = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tagged_paragraph_text("bar<a>baz").into(),
    ]);
    assert_slice(
        &case,
        Some("a"),
        None,
        &doc(vec![paragraph_text("baz")]),
        1,
        0,
    );

    let case = tagged_doc(vec![tagged_paragraph_text("hell<a>o wo<b>rld").into()]);
    assert_slice(&case, Some("a"), Some("b"), &paragraph_text("o wo"), 0, 0);

    let case = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    assert_slice(
        &case,
        Some("a"),
        Some("b"),
        &doc(vec![paragraph_text("e"), paragraph_text("t")]),
        1,
        1,
    );

    let em = schema_basic::em().unwrap();
    let case = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("here's noth").into(),
            tagged_marked_text("<a>ing and here's e<b>m", vec![em.clone()]).into(),
        ])
        .into(),
    ]);
    assert_slice(
        &case,
        Some("a"),
        Some("b"),
        &paragraph(vec![marked_text("ing and here's e", vec![em])]),
        0,
        0,
    );
}

#[test]
fn model_slice_matches_upstream_list_schema_cases() {
    // Mirrors the list-schema-specific slice cases from upstream
    // prosemirror-model/test/test-slice.ts that pine had previously deferred.

    // "can cut to a deep position":
    //   doc(blockquote(ul(li(p("a")), li(p("b<b>")))))  → cut at <b>
    //   expected: doc(blockquote(ul(li(p("a")), li(p("b"))))), openStart=0, openEnd=4
    let case = tagged_doc(vec![
        tagged_blockquote(vec![
            tagged_bullet_list(vec![
                tagged_list_item_text("a").into(),
                tagged_list_item(vec![tagged_paragraph_text("b<b>").into()]).into(),
            ])
            .into(),
        ])
        .into(),
    ]);
    let expected = doc(vec![
        schema_basic::blockquote(vec![bullet_list(vec![
            list_item_text("a"),
            list_item_text("b"),
        ])])
        .unwrap(),
    ]);
    assert_slice(&case, None, Some("b"), &expected, 0, 4);

    // "can cut from a deep position":
    //   doc(blockquote(ul(li(p("a")), li(p("<a>b")))))
    //   expected: doc(blockquote(ul(li(p("b"))))), openStart=4, openEnd=0
    let case = tagged_doc(vec![
        tagged_blockquote(vec![
            tagged_bullet_list(vec![
                tagged_list_item_text("a").into(),
                tagged_list_item(vec![tagged_paragraph_text("<a>b").into()]).into(),
            ])
            .into(),
        ])
        .into(),
    ]);
    let expected = doc(vec![
        schema_basic::blockquote(vec![bullet_list(vec![list_item_text("b")])]).unwrap(),
    ]);
    assert_slice(&case, Some("a"), None, &expected, 4, 0);

    // "can cut across different depths":
    //   doc(ul(li(p("hello")), li(p("wo<a>rld")), li(p("x"))), p(em("bo<b>o")))
    //   expected: doc(ul(li(p("rld")), li(p("x"))), p(em("bo"))), openStart=3, openEnd=1
    let em = schema_basic::em().unwrap();
    let case = tagged_doc(vec![
        tagged_bullet_list(vec![
            tagged_list_item_text("hello").into(),
            tagged_list_item(vec![tagged_paragraph_text("wo<a>rld").into()]).into(),
            tagged_list_item_text("x").into(),
        ])
        .into(),
        tagged_paragraph(vec![tagged_marked_text("bo<b>o", vec![em.clone()]).into()]).into(),
    ]);
    let expected = doc(vec![
        bullet_list(vec![list_item_text("rld"), list_item_text("x")]),
        paragraph(vec![marked_text("bo", vec![em])]),
    ]);
    assert_slice(&case, Some("a"), Some("b"), &expected, 3, 1);

    // "can cut between deeply nested nodes":
    //   doc(blockquote(p("foo<a>bar"), ul(li(p("a")), li(p("b"), "<b>", p("c"))), p("d")))
    //   expected: blockquote(p("bar"), ul(li(p("a")), li(p("b")))), openStart=1, openEnd=2
    let case = tagged_doc(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("foo<a>bar").into(),
            tagged_bullet_list(vec![
                tagged_list_item_text("a").into(),
                tagged_list_item(vec![
                    tagged_paragraph_text("b").into(),
                    tag("b"),
                    tagged_paragraph_text("c").into(),
                ])
                .into(),
            ])
            .into(),
            tagged_paragraph_text("d").into(),
        ])
        .into(),
    ]);
    let expected = schema_basic::blockquote(vec![
        paragraph_text("bar"),
        bullet_list(vec![list_item_text("a"), list_item_text("b")]),
    ])
    .unwrap();
    assert_slice(&case, Some("a"), Some("b"), &expected, 1, 2);
}

#[test]
fn model_slice_can_include_parent_context() {
    let case = tagged_doc(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("fo<a>o").into(),
            tagged_paragraph_text("bar<b>").into(),
        ])
        .into(),
    ]);
    let slice = case
        .node
        .slice_with_parents(case.tag("a"), case.tag("b"), true)
        .unwrap();

    assert_eq!(
        slice.content,
        Fragment::from(
            schema_basic::blockquote(vec![paragraph_text("o"), paragraph_text("bar")]).unwrap()
        )
    );
    assert_eq!(slice.open_start, 2);
    assert_eq!(slice.open_end, 2);
}

#[test]
fn model_nodes_between_matches_upstream_traversal_cases() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>bar<b>baz").into()]);
    assert_between(
        &tagged.node,
        tagged.tag("a"),
        tagged.tag("b"),
        &[
            ("paragraph", 0, Some("doc"), 0),
            ("foobarbaz", 1, Some("paragraph"), 0),
        ],
    );

    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![
            tagged_paragraph_text("f<a>oo").into(),
            tagged_paragraph_text("b").into(),
            tag("b"),
        ])
        .into(),
        tagged_paragraph_text("c").into(),
    ]);
    assert_between(
        &tagged.node,
        tagged.tag("a"),
        tagged.tag("b"),
        &[
            ("blockquote", 0, Some("doc"), 0),
            ("paragraph", 1, Some("blockquote"), 0),
            ("foo", 2, Some("paragraph"), 0),
            ("paragraph", 6, Some("blockquote"), 1),
            ("b", 7, Some("paragraph"), 0),
        ],
    );

    let doc = doc(vec![paragraph(vec![
        text("foo"),
        schema_basic::image("image.png", Option::<String>::None, Option::<String>::None).unwrap(),
        text("bar"),
        schema_basic::hard_break().unwrap(),
        text("quux"),
    ])]);
    assert_between(
        &doc,
        2,
        11,
        &[
            ("paragraph", 0, Some("doc"), 0),
            ("foo", 1, Some("paragraph"), 0),
            ("image", 4, Some("paragraph"), 1),
            ("bar", 5, Some("paragraph"), 2),
            ("hard_break", 8, Some("paragraph"), 3),
            ("quux", 9, Some("paragraph"), 4),
        ],
    );
}

#[test]
fn model_node_lookup_and_mark_range_helpers_follow_positions() {
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("a").into(),
            tagged_marked_text("<a>bc<b>", vec![em.clone()]).into(),
            tagged_text("d").into(),
        ])
        .into(),
    ]);
    let mark_from = tagged.tag("a");
    let mark_to = tagged.tag("b");
    let doc = tagged.node;

    assert_eq!(doc.node_at(0).unwrap().unwrap().type_name(), "paragraph");
    assert_eq!(doc.node_at(1).unwrap().unwrap().text(), Some("a"));
    assert!(doc.node_at(doc.content_size()).unwrap().is_none());

    let after_start = doc.child_after(0).unwrap();
    assert_eq!(after_start.index, 0);
    assert_eq!(after_start.offset, 0);
    assert_eq!(after_start.node.unwrap().type_name(), "paragraph");

    let before_end = doc.child_before(doc.content_size()).unwrap();
    assert_eq!(before_end.index, 0);
    assert_eq!(before_end.offset, 0);
    assert_eq!(before_end.node.unwrap().type_name(), "paragraph");

    assert!(doc.range_has_mark(mark_from, mark_to, &em).unwrap());
    assert!(doc.range_has_mark_type(mark_from, mark_to, "em").unwrap());
    assert!(!doc.range_has_mark_type(1, mark_from, "em").unwrap());
}

#[test]
fn model_can_replace_and_append_match_basic_content_rules() {
    let schema = schema_basic::schema();
    let paragraph = paragraph_text("one");
    let heading = heading(1, "two");
    let document = doc(vec![paragraph.clone()]);
    let text_fragment = Fragment::from(text("x"));
    let paragraph_fragment = Fragment::from(paragraph.clone());

    assert!(document.can_replace(0, 1, &paragraph_fragment, &schema));
    assert!(!document.can_replace(0, 1, &text_fragment, &schema));
    assert!(paragraph.can_replace(0, 1, &text_fragment, &schema));
    assert!(!paragraph.can_replace(0, 1, &paragraph_fragment, &schema));

    assert!(document.can_replace_with(0, 1, "paragraph", &[], &schema));
    assert!(!document.can_replace_with(0, 1, "text", &[], &schema));
    assert!(paragraph.can_append(&heading, &schema));

    let code = schema_basic::code().unwrap();
    assert!(!schema_basic::code_block("one").unwrap().can_replace(
        0,
        1,
        &Fragment::from(marked_text("x", vec![code])),
        &schema
    ));
}

#[test]
fn model_mark_exclusion_rules_match_upstream_custom_schema() {
    // `remark` mirrors upstream's `excludes: ""` (no exclusion at all, including
    // not excluding its own type). `user` excludes everything (`_`). `strong`
    // excludes the `em-group` (which `em` belongs to).
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("paragraph+"))
        .node(NodeSpec::new("paragraph").content("text*"))
        .node(NodeSpec::new("text").inline())
        .mark(MarkSpec::new("remark").excludes("").inclusive(false))
        .mark(MarkSpec::new("user").excludes("_"))
        .mark(MarkSpec::new("strong").excludes("em-group"))
        .mark(MarkSpec::new("em").group("em-group"))
        .finish()
        .unwrap();

    let remark1 = schema
        .mark("remark", {
            let mut attrs = Attrs::new();
            attrs.insert("id".to_string(), json!(1));
            attrs
        })
        .unwrap();
    let remark2 = schema
        .mark("remark", {
            let mut attrs = Attrs::new();
            attrs.insert("id".to_string(), json!(2));
            attrs
        })
        .unwrap();
    let user1 = schema
        .mark("user", {
            let mut attrs = Attrs::new();
            attrs.insert("id".to_string(), json!(1));
            attrs
        })
        .unwrap();
    let user2 = schema
        .mark("user", {
            let mut attrs = Attrs::new();
            attrs.insert("id".to_string(), json!(2));
            attrs
        })
        .unwrap();
    let custom_em = schema.mark("em", Attrs::new()).unwrap();
    let custom_strong = schema.mark("strong", Attrs::new()).unwrap();

    // allows nonexclusive instances of marks with the same type
    let combined = remark2
        .add_to_set(&schema, std::slice::from_ref(&remark1))
        .unwrap();
    assert!(Mark::same_set(
        &combined,
        &[remark1.clone(), remark2.clone()]
    ));

    // doesn't duplicate identical instances of nonexclusive marks
    let combined = remark1
        .add_to_set(&schema, std::slice::from_ref(&remark1))
        .unwrap();
    assert!(Mark::same_set(&combined, std::slice::from_ref(&remark1)));

    // a globally-excluding mark clears all others when added
    let combined = user1
        .add_to_set(&schema, &[remark1.clone(), custom_em.clone()])
        .unwrap();
    assert!(Mark::same_set(&combined, std::slice::from_ref(&user1)));

    // adding to a globally-excluding mark keeps the existing set
    let combined = custom_em
        .add_to_set(&schema, std::slice::from_ref(&user1))
        .unwrap();
    assert!(Mark::same_set(&combined, std::slice::from_ref(&user1)));

    // overwrites a globally-excluding mark with another instance
    let combined = user2
        .add_to_set(&schema, std::slice::from_ref(&user1))
        .unwrap();
    assert!(Mark::same_set(&combined, std::slice::from_ref(&user2)));

    // doesn't add anything when an existing mark excludes the added one
    let combined = custom_em
        .add_to_set(&schema, &[remark1.clone(), custom_strong.clone()])
        .unwrap();
    assert!(Mark::same_set(
        &combined,
        &[remark1.clone(), custom_strong.clone()]
    ));

    // removes excluded marks when adding a mark
    let combined = custom_strong
        .add_to_set(&schema, &[remark1.clone(), custom_em])
        .unwrap();
    assert!(Mark::same_set(&combined, &[remark1, custom_strong]));
}

#[test]
fn model_schema_basic_flags_defining_wrappers() {
    // Sanity check: schema_basic now marks the wrappers PM declares as
    // defining_for_content / defining in upstream's basic + list schemas.
    let schema = schema_basic::schema();
    assert!(
        schema
            .node_type("blockquote")
            .unwrap()
            .is_defining_for_content()
    );
    assert!(
        schema
            .node_type("blockquote")
            .unwrap()
            .is_defining_as_context()
    );
    assert!(
        schema
            .node_type("heading")
            .unwrap()
            .is_defining_for_content()
    );
    assert!(
        schema
            .node_type("heading")
            .unwrap()
            .is_defining_as_context()
    );
    assert!(
        schema
            .node_type("code_block")
            .unwrap()
            .is_defining_for_content()
    );
    assert!(
        schema
            .node_type("code_block")
            .unwrap()
            .is_defining_as_context()
    );
    assert!(
        schema
            .node_type("list_item")
            .unwrap()
            .is_defining_for_content()
    );
    assert!(
        schema
            .node_type("list_item")
            .unwrap()
            .is_defining_as_context()
    );
    assert!(
        !schema
            .node_type("paragraph")
            .unwrap()
            .is_defining_for_content()
    );
    assert!(!schema.node_type("hard_break").unwrap().is_selectable());
}

#[test]
fn model_schema_basic_declares_builtin_attrs() {
    let schema = schema_basic::schema();

    let heading = schema
        .node("heading", Attrs::new(), Fragment::empty())
        .unwrap();
    assert_eq!(heading.attrs().get("level"), Some(&json!(1)));

    let item = schema_basic::list_item(vec![paragraph_text("one")]).unwrap();
    let ordered = schema
        .node("ordered_list", Attrs::new(), Fragment::from(item))
        .unwrap();
    assert_eq!(ordered.attrs().get("order"), Some(&json!(1)));

    let missing_image_src = schema
        .node("image", Attrs::new(), Fragment::empty())
        .unwrap_err();
    assert!(
        missing_image_src
            .to_string()
            .contains("missing required attribute src")
    );
    let image =
        schema_basic::image("image.png", Option::<String>::None, Option::<String>::None).unwrap();
    assert_eq!(image.attrs().get("src"), Some(&json!("image.png")));
    assert_eq!(image.attrs().get("alt"), Some(&json!(null)));
    assert_eq!(image.attrs().get("title"), Some(&json!(null)));

    let missing_link_href = schema.mark("link", Attrs::new()).unwrap_err();
    assert!(
        missing_link_href
            .to_string()
            .contains("missing required attribute href")
    );
    let link = schema_basic::link("https://example.test", Option::<String>::None).unwrap();
    assert_eq!(
        link.attrs().get("href"),
        Some(&json!("https://example.test"))
    );
    assert_eq!(link.attrs().get("title"), Some(&json!(null)));

    let task = schema_basic::task_item(false, vec![paragraph_text("todo")]).unwrap();
    assert_eq!(task.attrs().get("checked"), Some(&json!(false)));
}

#[test]
fn model_attribute_spec_fills_defaults_and_requires_missing() {
    use pine_richtext::model::AttributeSpec;
    // Schema: a doc containing pages that need a required `id` and an
    // optional `title` with a default.
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("page+"))
        .node(
            NodeSpec::new("page")
                .content("text*")
                .required_attr("id")
                .attr("title", json!("Untitled")),
        )
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();

    // Required attr missing — rejected.
    let err = schema
        .node("page", Attrs::new(), Fragment::empty())
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("missing required"));

    // Required provided, default filled in.
    let mut attrs = Attrs::new();
    attrs.insert("id".to_string(), json!("p1"));
    let page = schema.node("page", attrs, Fragment::empty()).unwrap();
    assert_eq!(page.attrs().get("id"), Some(&json!("p1")));
    assert_eq!(page.attrs().get("title"), Some(&json!("Untitled")));

    // Caller-supplied value overrides the default.
    let mut attrs = Attrs::new();
    attrs.insert("id".to_string(), json!("p2"));
    attrs.insert("title".to_string(), json!("Cover"));
    attrs.insert("ignored".to_string(), json!(true));
    let page = schema.node("page", attrs, Fragment::empty()).unwrap();
    assert_eq!(page.attrs().get("id"), Some(&json!("p2")));
    assert_eq!(page.attrs().get("title"), Some(&json!("Cover")));
    assert_eq!(page.attrs().get("ignored"), None);

    // AttributeSpec accessors round-trip.
    let optional = AttributeSpec::with_default(json!(0));
    let required = AttributeSpec::required();
    assert!(optional.has_default());
    assert_eq!(optional.default_value(), Some(&json!(0)));
    assert!(!required.has_default());
}

#[test]
fn model_resolved_block_range_respects_predicate() {
    // blockRange should walk outward past depths whose parent doesn't satisfy
    // the predicate. Matches upstream's `ResolvedPos.blockRange(other, pred)`.
    let document = doc(vec![
        schema_basic::blockquote(vec![paragraph_text("foo")]).unwrap(),
    ]);
    // Position 3 sits inside the paragraph's text. Default blockRange returns
    // a range whose parent is the blockquote (depth 1).
    let resolved = document.resolve(3).unwrap();
    let plain_range = resolved.block_range(None).unwrap();
    assert_eq!(plain_range.parent().type_name(), "blockquote");

    // A predicate that only accepts the top-level doc walks outward.
    let doc_range = resolved
        .block_range_with(None, |node| node.type_name() == "doc")
        .unwrap();
    assert_eq!(doc_range.parent().type_name(), "doc");

    // A predicate that nothing satisfies returns None.
    assert!(
        resolved
            .block_range_with(None, |node| node.type_name() == "missing")
            .is_none()
    );
}

#[test]
fn model_node_cut_matches_upstream_cases() {
    // extracts a full block: doc(p("foo"), <a>, p("bar"), <b>, p("baz")) -> doc(p("bar"))
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tag("a"),
        tagged_paragraph_text("bar").into(),
        tag("b"),
        tagged_paragraph_text("baz").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let cut = tagged.node.cut(from, to).unwrap();
    assert_eq!(cut, doc(vec![paragraph_text("bar")]));

    // cut text mid-paragraph: doc(p("0"), p("foo<a>bar<b>baz"), p("2")) -> doc(p("bar"))
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("0").into(),
        tagged_paragraph_text("foo<a>bar<b>baz").into(),
        tagged_paragraph_text("2").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let cut = tagged.node.cut(from, to).unwrap();
    assert_eq!(cut, doc(vec![paragraph_text("bar")]));

    // cuts from the left
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("foo<b>bar").into()]).into(),
    ]);
    let to = tagged.tag("b");
    let cut = tagged.node.cut(0, to).unwrap();
    assert_eq!(
        cut,
        doc(vec![
            schema_basic::blockquote(vec![paragraph_text("foo")]).unwrap()
        ])
    );

    // cuts to the right
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("foo<a>bar").into()]).into(),
    ]);
    let from = tagged.tag("a");
    let cut = tagged.node.cut(from, tagged.node.content_size()).unwrap();
    assert_eq!(
        cut,
        doc(vec![
            schema_basic::blockquote(vec![paragraph_text("bar")]).unwrap()
        ])
    );
}

#[test]
fn model_node_text_content_matches_upstream_cases() {
    assert_eq!(doc(vec![paragraph_text("foo")]).text_content(), "foo");
    assert_eq!(text("foo").text_content(), "foo");

    let em = schema_basic::em().unwrap();
    let nested = doc(vec![paragraph(vec![marked_text("a", vec![em]), text("b")])]);
    assert_eq!(nested.text_content(), "ab");
}

#[test]
fn model_node_check_rejects_invalid_content_and_marks() {
    let schema = schema_basic::schema();

    // Top-level doc rejects an inline text child during construction.
    let err = schema
        .node("doc", Attrs::new(), Fragment::from(text("x")))
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("cannot contain"),
        "expected content-error, got: {err}"
    );

    // code_block disallows marks on its children.
    let code = schema_basic::code().unwrap();
    let result =
        pine_richtext::transform::Transform::new(schema.clone(), doc(vec![code_block("hello")]))
            .add_mark(1, 5, code)
            .map(|_| ());
    assert!(result.is_err());

    // Paragraph cannot directly contain a block (blockquote).
    let nested = schema_basic::blockquote(vec![empty_paragraph()]).unwrap();
    let err = schema
        .node("paragraph", Attrs::new(), Fragment::from(nested))
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("cannot contain"));
}

#[test]
fn model_fragment_from_joins_adjacent_text() {
    let frag = Fragment::from(vec![text("a"), text("b")]);
    assert_eq!(frag.len(), 1);
    let only = frag.child(0).unwrap();
    assert_eq!(only.text(), Some("ab"));
}

#[test]
fn model_node_json_round_trip_preserves_nested_marks() {
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let document = doc(vec![paragraph(vec![
        text("foo"),
        marked_text("bar", vec![em, strong]),
    ])]);
    let json = serde_json::to_value(&document).unwrap();
    let decoded: Node = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, document);
}

#[test]
fn model_replace_reports_invalid_fit_errors() {
    // doesn't allow the left side to be too deep
    let tagged = tagged_doc(vec![tagged_paragraph(vec![tag("a"), tag("b")]).into()]);
    let insert = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph(vec![tag("a")]).into()]).into(),
        tag("b"),
    ]);
    assert_replace_error(tagged, Some(insert), "deeper");

    // depth mismatch
    let tagged = tagged_doc(vec![tagged_paragraph(vec![tag("a"), tag("b")]).into()]);
    let insert = tagged_doc(vec![tag("a"), tagged_paragraph(vec![tag("b")]).into()]);
    assert_replace_error(tagged, Some(insert), "inconsistent");
}

#[test]
fn model_replace_rejects_bad_fit_and_unjoinable_content() {
    let schema = schema_basic::schema();

    // Trying to insert a paragraph slice directly into the doc top-level via
    // a closed-end slice with content the doc rejects fails with an invalid
    // content error.
    let target = doc(vec![empty_paragraph()]);
    let bad_slice = Slice::new(Fragment::from(text("hi")), 0, 0);
    let err = pine_richtext::transform::replace(&target, 0, 0, bad_slice, &schema).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("invalid") || msg.contains("cannot contain"),
        "expected invalid-content error, got: {err}"
    );

    // Deleting a range that crosses an unjoinable boundary (bullet_list ↔
    // blockquote at the same depth, different wrapper types) errors with
    // "cannot join". Mirrors upstream test-replace's "rejects an unjoinable
    // delete":
    //   doc(blockquote(p("a"), "<a>"), ul("<b>", li(p("b"))))
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("a").into(), tag("a")]).into(),
        tagged_bullet_list(vec![tag("b"), tagged_list_item_text("b").into()]).into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let err = pine_richtext::transform::replace(&tagged.node, from, to, Slice::empty(), &schema)
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("cannot join") || msg.contains("cannot contain") || msg.contains("invalid"),
        "expected join/content error, got: {err}"
    );
}

#[test]
fn model_replace_rejects_unjoinable_list_schema_content() {
    // Mirrors upstream test-replace's "rejects unjoinable content":
    //   doc(ul(li(p("a")), "<a>"), "<b>") paste doc(p("foo", "<a>"), "<b>")
    // The destination's right side (end-of-doc after ul) can't absorb the
    // slice's open-start-inside-p content because ul's surroundings have
    // no p to join with.
    let schema = schema_basic::schema();

    let target = tagged_doc(vec![
        tagged_bullet_list(vec![tagged_list_item_text("a").into(), tag("a")]).into(),
        tag("b"),
    ]);
    let source = tagged_doc(vec![
        tagged_paragraph(vec![tagged_text("foo").into(), tag("a")]).into(),
        tag("b"),
    ]);
    let slice = source.node.slice(source.tag("a"), source.tag("b")).unwrap();
    let from = target.tag("a");
    let to = target.tag("b");
    let err =
        pine_richtext::transform::replace(&target.node, from, to, slice, &schema).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("cannot join") || msg.contains("invalid") || msg.contains("cannot contain"),
        "expected join/content error, got: {err}"
    );
}

#[test]
fn model_replace_supports_top_level_join_on_delete() {
    let schema = schema_basic::schema();

    // joins on delete (no slice)
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let result =
        pine_richtext::transform::replace(&tagged.node, from, to, Slice::empty(), &schema).unwrap();
    assert_eq!(result, doc(vec![paragraph_text("onwo")]));

    // can replace within a block
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("a<a>bc<b>d").into()]).into(),
    ]);
    let insert = tagged_doc(vec![tagged_paragraph_text("x<a>y<b>z").into()]);
    let slice = insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap();
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let result = pine_richtext::transform::replace(&tagged.node, from, to, slice, &schema).unwrap();
    assert_eq!(
        result,
        doc(vec![
            schema_basic::blockquote(vec![paragraph_text("ayd")]).unwrap()
        ])
    );
}

#[test]
fn model_slice_can_cut_across_paragraphs_and_marks() {
    let em = schema_basic::em().unwrap();

    // can cut across paragraphs
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("on<a>e").into(),
        tagged_paragraph_text("t<b>wo").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let slice = tagged.node.slice(from, to).unwrap();
    assert_eq!(slice.open_start, 1);
    assert_eq!(slice.open_end, 1);
    assert_eq!(
        &slice.content,
        doc(vec![paragraph_text("e"), paragraph_text("t")]).content()
    );

    // can cut part of marked text
    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("here's noth").into(),
            tag("a"),
            tagged_text("ing and ").into(),
            tagged_marked_text("here's e<b>m", vec![em.clone()]).into(),
        ])
        .into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let slice = tagged.node.slice(from, to).unwrap();
    assert_eq!(slice.open_start, 0);
    assert_eq!(slice.open_end, 0);
    assert_eq!(
        &slice.content,
        paragraph(vec![text("ing and "), marked_text("here's e", vec![em])]).content()
    );
}

// ---------- Node.toString / debug rendering ----------

#[test]
fn model_node_to_string_nests_block_children() {
    // Mirrors test-node.ts "nests": doc(ul(li(p("hey"), p()), li(p("foo"))))
    // renders as `doc(bullet_list(list_item(paragraph("hey"), paragraph), list_item(paragraph("foo"))))`.
    let document = doc(vec![bullet_list(vec![
        list_item(vec![paragraph_text("hey"), empty_paragraph()]),
        list_item_text("foo"),
    ])]);
    assert_eq!(
        document.to_string(),
        r#"doc(bullet_list(list_item(paragraph("hey"), paragraph), list_item(paragraph("foo"))))"#,
    );
}

#[test]
fn model_node_to_string_shows_inline_children() {
    // Mirrors test-node.ts "shows inline children": doc(p("foo", img(), br(), "bar")).
    let document = doc(vec![paragraph(vec![
        text("foo"),
        image("img.png"),
        hard_break(),
        text("bar"),
    ])]);
    assert_eq!(
        document.to_string(),
        r#"doc(paragraph("foo", image, hard_break, "bar"))"#,
    );
}

#[test]
fn model_node_to_string_wraps_marks() {
    // Mirrors test-node.ts "shows marks": text-with-marks renders with each
    // mark wrapping the inner string, outer-to-inner.
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let code = schema_basic::code().unwrap();
    let document = doc(vec![paragraph(vec![
        text("foo"),
        marked_text("bar", vec![em.clone()]),
        marked_text("quux", vec![em, strong]),
        marked_text("baz", vec![code]),
    ])]);
    assert_eq!(
        document.to_string(),
        r#"doc(paragraph("foo", em("bar"), em(strong("quux")), code("baz")))"#,
    );
}

// ---------- Node.text_between_with: leaf-text callback ----------

#[test]
fn model_text_between_with_custom_leaf_callback() {
    // Mirrors test-node.ts "works when passing a custom function as leafText":
    //   doc(p("foo", img(), br())).textBetween(..., (node) => …)
    //   → 'foo<image><break>'
    let document = doc(vec![paragraph(vec![
        text("foo"),
        image("x.png"),
        hard_break(),
    ])]);
    let leaf = |node: &Node| match node.type_name() {
        "image" => "<image>".to_string(),
        "hard_break" => "<break>".to_string(),
        _ => String::new(),
    };
    let result = document
        .text_between_with(0, document.content_size(), "", Some(&leaf))
        .unwrap();
    assert_eq!(result, "foo<image><break>");
}

#[test]
fn model_text_between_uses_node_spec_leaf_text_hook() {
    // Mirrors test-node.ts "works with leafText": a custom schema declares a
    // `contact` leaf with a `leaf_text` hook that formats `name <email>`.
    fn contact_leaf_text(node: &Node) -> String {
        let name = node
            .attrs()
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let email = node
            .attrs()
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("{name} <{email}>")
    }
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("paragraph+"))
        .node(NodeSpec::new("paragraph").content("(text|contact)*"))
        .node(NodeSpec::new("text").inline())
        .node(
            NodeSpec::new("contact")
                .inline()
                .atom()
                .required_attr("name")
                .required_attr("email")
                .leaf_text(contact_leaf_text),
        )
        .finish()
        .unwrap();

    let mut attrs = Attrs::new();
    attrs.insert("name".to_string(), json!("Alice"));
    attrs.insert("email".to_string(), json!("alice@example.com"));
    let contact = schema.node("contact", attrs, Fragment::empty()).unwrap();
    let paragraph = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(vec![schema.text("Hello ", Vec::new()).unwrap(), contact]),
        )
        .unwrap();
    let document = schema
        .node("doc", Attrs::new(), Fragment::from(paragraph))
        .unwrap();

    // Caller supplies the leaf callback via Schema::leaf_text_for, which
    // consults the NodeSpec.leaf_text hook. Without a callback, leaf nodes
    // contribute nothing.
    let leaf = |node: &Node| schema.leaf_text_for(node).unwrap_or_default();
    let result = document
        .text_between_with(0, document.content_size(), "", Some(&leaf))
        .unwrap();
    assert_eq!(result, "Hello Alice <alice@example.com>");
}

#[test]
fn model_text_between_with_custom_callback_overrides_spec_leaf_text() {
    // Mirrors test-node.ts "should ignore leafText when passing a custom
    // leafText": the caller's explicit callback takes precedence over any
    // `NodeSpec.leaf_text` hook. (In pine, this is naturally true: callers
    // decide whether to consult the schema or not.)
    fn contact_leaf_text(node: &Node) -> String {
        let name = node
            .attrs()
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let email = node
            .attrs()
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("{name} <{email}>")
    }
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("paragraph+"))
        .node(NodeSpec::new("paragraph").content("(text|contact)*"))
        .node(NodeSpec::new("text").inline())
        .node(
            NodeSpec::new("contact")
                .inline()
                .atom()
                .required_attr("name")
                .required_attr("email")
                .leaf_text(contact_leaf_text),
        )
        .finish()
        .unwrap();

    let mut attrs = Attrs::new();
    attrs.insert("name".to_string(), json!("Alice"));
    attrs.insert("email".to_string(), json!("alice@example.com"));
    let contact = schema.node("contact", attrs, Fragment::empty()).unwrap();
    let paragraph = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(vec![schema.text("Hello ", Vec::new()).unwrap(), contact]),
        )
        .unwrap();
    let document = schema
        .node("doc", Attrs::new(), Fragment::from(paragraph))
        .unwrap();

    // Force every leaf to render as "<anonymous>" — the spec hook is
    // bypassed because the caller chose not to forward to it.
    let leaf = |_node: &Node| "<anonymous>".to_string();
    let result = document
        .text_between_with(0, document.content_size(), "", Some(&leaf))
        .unwrap();
    assert_eq!(result, "Hello <anonymous>");
}
