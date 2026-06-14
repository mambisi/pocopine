//! Parity tests for `prosemirror-model/test/test-mark.ts` and
//! `prosemirror-model/test/test-diff.ts` cases that pine's main parity file
//! had previously deferred.
//!
//! These tests are split into a separate file because they form coherent
//! domain groups (mark-set algebra and fragment diff search) that read
//! better as a dedicated suite. The mark tests use both `schema_basic` and a
//! custom schema with non-inclusive / excludes-`""` / globally-excluding
//! marks (mirroring upstream's `customSchema`). The diff tests exercise
//! `find_diff_start` and `find_diff_end` against pairs of fragments tagged
//! with `<a>` to anchor the expected diff position.

use serde_json::json;

mod support;

use pine_richtext::model::{
    Attrs, Fragment, Mark, MarkSpec, NodeSpec, Schema, find_diff_end, find_diff_start,
};
use pine_richtext::schema_basic;

use support::{
    doc, heading, paragraph, paragraph_text, tag, tagged_blockquote, tagged_doc,
    tagged_heading_text, tagged_marked_text, tagged_paragraph, tagged_paragraph_text, tagged_text,
    text,
};

// ---------- Mark::eq, Mark::same_set, addToSet, removeFromSet ----------

#[test]
fn mark_eq_compares_attributes_and_titles() {
    // Mirrors test-mark.ts "Mark.eq" describe block.
    let link_foo = schema_basic::link("http://foo", Option::<String>::None).unwrap();
    let link_foo_b = schema_basic::link("http://foo", Option::<String>::None).unwrap();
    let link_bar = schema_basic::link("http://bar", Option::<String>::None).unwrap();
    let link_foo_titled_a = schema_basic::link("http://foo", Some("A")).unwrap();
    let link_foo_titled_b = schema_basic::link("http://foo", Some("B")).unwrap();

    // identical links are equal
    assert_eq!(link_foo, link_foo_b);

    // different href → not equal
    assert_ne!(link_foo, link_bar);

    // same href, different title → not equal
    assert_ne!(link_foo_titled_a, link_foo_titled_b);
}

#[test]
fn mark_same_set_handles_size_mismatches_and_identical_links() {
    // Mirrors test-mark.ts "Mark.sameSet" describe block, covering the size
    // mismatch and identical-links cases pine didn't have explicit asserts for.
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let code = schema_basic::code().unwrap();
    let link_foo = schema_basic::link("http://foo", Option::<String>::None).unwrap();
    let link_bar = schema_basic::link("http://bar", Option::<String>::None).unwrap();

    // size differs
    assert!(!Mark::same_set(
        &[em.clone(), strong.clone()],
        &[em.clone(), strong.clone(), code.clone()]
    ));

    // identical links in set
    assert!(Mark::same_set(
        &[link_foo.clone(), code.clone()],
        &[link_foo.clone(), code.clone()],
    ));

    // different links in set
    assert!(!Mark::same_set(
        &[link_foo, code.clone()],
        &[link_bar, code],
    ));
}

#[test]
fn mark_add_to_set_orders_by_rank_and_handles_no_ops() {
    // Mirrors test-mark.ts "addToSet" ordering and no-op cases. Uses
    // schema_basic for the no-op + simple-ordering cases.
    let schema = schema_basic::schema();
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let link_foo = schema_basic::link("http://foo", Option::<String>::None).unwrap();

    // no-op when the added mark is already in the set
    assert!(Mark::same_set(
        &em.add_to_set(&schema, std::slice::from_ref(&em)).unwrap(),
        std::slice::from_ref(&em),
    ));

    // lower-rank em goes before higher-rank strong
    assert!(Mark::same_set(
        &em.add_to_set(&schema, std::slice::from_ref(&strong))
            .unwrap(),
        &[em.clone(), strong.clone()],
    ));

    // higher-rank strong goes after em
    assert!(Mark::same_set(
        &strong
            .add_to_set(&schema, std::slice::from_ref(&em))
            .unwrap(),
        &[em.clone(), strong.clone()],
    ));

    // adding an existing link is a no-op
    assert!(Mark::same_set(
        &link_foo
            .add_to_set(&schema, &[em.clone(), link_foo.clone()])
            .unwrap(),
        &[em, link_foo],
    ));
}

#[test]
fn mark_add_to_set_orders_code_at_end_and_middle_rank_between() {
    // Mirrors test-mark.ts "puts code marks at the end" and "puts marks with
    // middle rank in the middle" — these REQUIRE a schema where code is a
    // regular mark without `excludes`. Pine's schema_basic deliberately
    // gives code excludes("_") (drops every other mark), so these cases
    // run against a custom schema mirroring upstream's basic shape.
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("paragraph+"))
        .node(NodeSpec::new("paragraph").content("text*"))
        .node(NodeSpec::new("text").inline())
        .mark(MarkSpec::new("link").inclusive(false))
        .mark(MarkSpec::new("em"))
        .mark(MarkSpec::new("strong"))
        .mark(MarkSpec::new("code"))
        .finish()
        .unwrap();
    let em = schema.mark("em", Attrs::new()).unwrap();
    let strong = schema.mark("strong", Attrs::new()).unwrap();
    let code = schema.mark("code", Attrs::new()).unwrap();
    let link_foo = schema
        .mark("link", [("href".to_string(), json!("http://foo"))].into())
        .unwrap_or_else(|_| schema.mark("link", Attrs::new()).unwrap());

    // code (highest rank) goes at the very end
    assert!(Mark::same_set(
        &code
            .add_to_set(&schema, &[em.clone(), strong.clone(), link_foo.clone()])
            .unwrap(),
        &[em.clone(), strong.clone(), link_foo, code.clone()],
    ));

    // middle-rank strong lands between em (lower) and code (higher)
    assert!(Mark::same_set(
        &strong
            .add_to_set(&schema, &[em.clone(), code.clone()])
            .unwrap(),
        &[em, strong, code],
    ));
}

#[test]
fn mark_remove_from_set_handles_no_ops_and_attr_sensitivity() {
    // Mirrors test-mark.ts "removeFromSet" cases beyond pine's existing
    // coverage. Focuses on the no-op shapes and attr-sensitivity rule that
    // a 1-arg removeFromSet must respect (same href + different title MUST
    // NOT match — full equality, not just type-name equality).
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let link_foo = schema_basic::link("http://foo", Option::<String>::None).unwrap();
    let link_foo_titled = schema_basic::link("http://foo", Some("title")).unwrap();

    // empty set in → empty set out
    assert!(Mark::same_set(&em.remove_from_set(&[]), &[]));

    // remove the last mark from a set
    assert!(Mark::same_set(
        &em.remove_from_set(std::slice::from_ref(&em)),
        &[],
    ));

    // no-op when the mark isn't in the set
    assert!(Mark::same_set(
        &strong.remove_from_set(std::slice::from_ref(&em)),
        std::slice::from_ref(&em),
    ));

    // remove a mark with attributes (same attrs)
    assert!(Mark::same_set(
        &link_foo.remove_from_set(std::slice::from_ref(&link_foo)),
        &[],
    ));

    // doesn't remove when attrs differ (title="title" must not match titleless)
    assert!(Mark::same_set(
        &link_foo_titled.remove_from_set(std::slice::from_ref(&link_foo)),
        std::slice::from_ref(&link_foo),
    ));
}

// ---------- ResolvedPos.marks on a custom schema with non-inclusive marks ----

#[test]
fn resolved_pos_marks_omits_non_inclusive_at_attr_change_boundary() {
    // Mirrors test-mark.ts "ResolvedPos.marks" cases that exercise the
    // non-inclusive boundary rule on a custom schema (the last case —
    // "excludes non-inclusive marks at a point where mark attrs change").
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("paragraph+"))
        .node(NodeSpec::new("paragraph").content("text*"))
        .node(NodeSpec::new("text").inline())
        .mark(MarkSpec::new("remark").excludes("").inclusive(false))
        .finish()
        .unwrap();

    let remark1 = schema
        .mark("remark", [("id".to_string(), json!(1))].into())
        .unwrap();
    let remark2 = schema
        .mark("remark", [("id".to_string(), json!(2))].into())
        .unwrap();

    // doc: paragraph(text("one"|remark2), text("two"|remark1))
    let document = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(
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
            ),
        )
        .unwrap();

    // At the boundary between the two text nodes (pos 4): both runs carry
    // non-inclusive `remark` marks, but the attrs differ. PM omits the
    // non-inclusive mark entirely in this case (you don't "carry over" a
    // remark when its identity is changing).
    let resolved = document.resolve(4).unwrap();
    assert!(Mark::same_set(&resolved.marks(&schema), &[]));
}

// ---------- find_diff_start / find_diff_end ----------

fn find_start(left: &support::TaggedNode, right: &pine_richtext::model::Node) -> Option<usize> {
    find_diff_start(left.node.content(), right.content(), 0)
}

fn assert_diff_start_at_tag(left: support::TaggedNode, right: pine_richtext::model::Node) {
    let expected = left.tag("a");
    assert_eq!(find_start(&left, &right), Some(expected));
}

fn assert_diff_start_none(left: pine_richtext::model::Node, right: pine_richtext::model::Node) {
    assert_eq!(
        find_diff_start(left.content(), right.content(), 0),
        None,
        "expected identical fragments to report no diff",
    );
}

#[test]
fn diff_start_returns_none_for_identical_nodes() {
    let em = schema_basic::em().unwrap();
    let h1_bye = heading(1, "bye");
    let left = doc(vec![
        paragraph(vec![
            text("a"),
            pine_richtext::model::Node::clone(&{
                // marked "b"
                let mark = em.clone();
                support::marked_text("b", vec![mark])
            }),
        ]),
        paragraph_text("hello"),
        pine_richtext::schema_basic::blockquote(vec![h1_bye.clone()]).unwrap(),
    ]);
    let right = doc(vec![
        paragraph(vec![text("a"), support::marked_text("b", vec![em])]),
        paragraph_text("hello"),
        pine_richtext::schema_basic::blockquote(vec![h1_bye]).unwrap(),
    ]);
    assert_diff_start_none(left, right);
}

#[test]
fn diff_start_notices_when_one_node_is_longer() {
    // doc(p("a", em("b")), p("hello"), bq(h1("bye")), <a>) vs
    // doc(p("a", em("b")), p("hello"), bq(h1("bye")), p("oops"))
    // Tag <a> sits at the boundary AFTER the third child of doc.
    let em = schema_basic::em().unwrap();
    let bq = pine_richtext::schema_basic::blockquote(vec![heading(1, "bye")]).unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("a").into(),
            tagged_marked_text("b", vec![em.clone()]).into(),
        ])
        .into(),
        tagged_paragraph_text("hello").into(),
        tagged_blockquote(vec![tagged_heading_text(1, "bye").into()]).into(),
        tag("a"),
    ]);
    let right = doc(vec![
        paragraph(vec![text("a"), support::marked_text("b", vec![em])]),
        paragraph_text("hello"),
        bq,
        paragraph_text("oops"),
    ]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_notices_when_one_node_is_shorter() {
    let em = schema_basic::em().unwrap();
    let bq = pine_richtext::schema_basic::blockquote(vec![heading(1, "bye")]).unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("a").into(),
            tagged_marked_text("b", vec![em.clone()]).into(),
        ])
        .into(),
        tagged_paragraph_text("hello").into(),
        tagged_blockquote(vec![tagged_heading_text(1, "bye").into()]).into(),
        tag("a"),
        tagged_paragraph_text("oops").into(),
    ]);
    let right = doc(vec![
        paragraph(vec![text("a"), support::marked_text("b", vec![em])]),
        paragraph_text("hello"),
        bq,
    ]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_notices_differing_marks() {
    // doc(p("a<a>", em("b"))) vs doc(p("a", strong("b")))
    // Tag <a> sits right after "a", at the position where the marks differ.
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("a<a>").into(),
            tagged_marked_text("b", vec![em]).into(),
        ])
        .into(),
    ]);
    let right = doc(vec![paragraph(vec![
        text("a"),
        support::marked_text("b", vec![strong]),
    ])]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_stops_at_longer_text() {
    // doc(p("foo<a>bar", em("b"))) vs doc(p("foo", em("b")))
    let em = schema_basic::em().unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("foo<a>bar").into(),
            tagged_marked_text("b", vec![em.clone()]).into(),
        ])
        .into(),
    ]);
    let right = doc(vec![paragraph(vec![
        text("foo"),
        support::marked_text("b", vec![em]),
    ])]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_stops_at_a_different_character() {
    // doc(p("foo<a>bar")) vs doc(p("foocar"))
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![tagged_text("foo<a>bar").into()]).into(),
    ]);
    let right = doc(vec![paragraph_text("foocar")]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_stops_at_a_different_node_type() {
    // doc(p("a"), <a>, p("b")) vs doc(p("a"), h1("b"))
    let left_tagged = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tag("a"),
        tagged_paragraph_text("b").into(),
    ]);
    let right = doc(vec![paragraph_text("a"), heading(1, "b")]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_works_when_the_difference_is_at_the_start() {
    // doc(<a>, p("b")) vs doc(h1("b"))
    let left_tagged = tagged_doc(vec![tag("a"), tagged_paragraph_text("b").into()]);
    let right = doc(vec![heading(1, "b")]);
    assert_diff_start_at_tag(left_tagged, right);
}

#[test]
fn diff_start_notices_a_different_attribute() {
    // doc(p("a"), <a>, h1("foo")) vs doc(p("a"), h2("foo"))
    let left_tagged = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tag("a"),
        tagged_heading_text(1, "foo").into(),
    ]);
    let right = doc(vec![paragraph_text("a"), heading(2, "foo")]);
    assert_diff_start_at_tag(left_tagged, right);
}

fn assert_diff_end_at_tag(left: support::TaggedNode, right: pine_richtext::model::Node) {
    // PM's findDiffEnd reports the *position before which both fragments
    // still match going from the end backwards*. Pine's find_diff_end takes
    // base offsets (0, 0) and returns (a_pos, b_pos) in that coordinate
    // system — the same number PM's `found.a` carries.
    let expected = left.tag("a");
    let found = find_diff_end(left.node.content(), right.content(), 0, 0);
    let (left_end, _) = found.expect("expected a diff end");
    assert_eq!(left_end, expected);
}

#[test]
fn diff_end_returns_none_when_there_is_no_difference() {
    let em = schema_basic::em().unwrap();
    let left = doc(vec![
        paragraph(vec![text("a"), support::marked_text("b", vec![em.clone()])]),
        paragraph_text("hello"),
        pine_richtext::schema_basic::blockquote(vec![heading(1, "bye")]).unwrap(),
    ]);
    let right = doc(vec![
        paragraph(vec![text("a"), support::marked_text("b", vec![em])]),
        paragraph_text("hello"),
        pine_richtext::schema_basic::blockquote(vec![heading(1, "bye")]).unwrap(),
    ]);
    assert_eq!(
        find_diff_end(
            left.content(),
            right.content(),
            left.content_size(),
            right.content_size(),
        ),
        None,
    );
}

#[test]
fn diff_end_notices_when_the_second_doc_is_longer() {
    let em = schema_basic::em().unwrap();
    let left_tagged = tagged_doc(vec![
        tag("a"),
        tagged_paragraph(vec![
            tagged_text("a").into(),
            tagged_marked_text("b", vec![em.clone()]).into(),
        ])
        .into(),
        tagged_paragraph_text("hello").into(),
        tagged_blockquote(vec![tagged_heading_text(1, "bye").into()]).into(),
    ]);
    let right = doc(vec![
        paragraph_text("oops"),
        paragraph(vec![text("a"), support::marked_text("b", vec![em])]),
        paragraph_text("hello"),
        pine_richtext::schema_basic::blockquote(vec![heading(1, "bye")]).unwrap(),
    ]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_notices_when_the_second_doc_is_shorter() {
    let em = schema_basic::em().unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph_text("oops").into(),
        tag("a"),
        tagged_paragraph(vec![
            tagged_text("a").into(),
            tagged_marked_text("b", vec![em.clone()]).into(),
        ])
        .into(),
        tagged_paragraph_text("hello").into(),
        tagged_blockquote(vec![tagged_heading_text(1, "bye").into()]).into(),
    ]);
    let right = doc(vec![
        paragraph(vec![text("a"), support::marked_text("b", vec![em])]),
        paragraph_text("hello"),
        pine_richtext::schema_basic::blockquote(vec![heading(1, "bye")]).unwrap(),
    ]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_notices_different_styles() {
    // doc(p("a", em("b"), "<a>c")) vs doc(p("a", strong("b"), "c"))
    let em = schema_basic::em().unwrap();
    let strong = schema_basic::strong().unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("a").into(),
            tagged_marked_text("b", vec![em]).into(),
            tagged_text("<a>c").into(),
        ])
        .into(),
    ]);
    let right = doc(vec![paragraph(vec![
        text("a"),
        support::marked_text("b", vec![strong]),
        text("c"),
    ])]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_spots_longer_text() {
    // doc(p("bar<a>foo", em("b"))) vs doc(p("foo", em("b")))
    let em = schema_basic::em().unwrap();
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("bar<a>foo").into(),
            tagged_marked_text("b", vec![em.clone()]).into(),
        ])
        .into(),
    ]);
    let right = doc(vec![paragraph(vec![
        text("foo"),
        support::marked_text("b", vec![em]),
    ])]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_spots_different_text() {
    // doc(p("foob<a>ar")) vs doc(p("foocar"))
    let left_tagged = tagged_doc(vec![
        tagged_paragraph(vec![tagged_text("foob<a>ar").into()]).into(),
    ]);
    let right = doc(vec![paragraph_text("foocar")]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_notices_different_nodes() {
    // doc(p("a"), <a>, p("b")) vs doc(h1("a"), p("b"))
    let left_tagged = tagged_doc(vec![
        tagged_paragraph_text("a").into(),
        tag("a"),
        tagged_paragraph_text("b").into(),
    ]);
    let right = doc(vec![heading(1, "a"), paragraph_text("b")]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_notices_a_difference_at_the_end() {
    // doc(p("b"), <a>) vs doc(h1("b"))
    let left_tagged = tagged_doc(vec![tagged_paragraph_text("b").into(), tag("a")]);
    let right = doc(vec![heading(1, "b")]);
    assert_diff_end_at_tag(left_tagged, right);
}

#[test]
fn diff_end_handles_a_similar_start() {
    // doc(<a>, p("hello")) vs doc(p("hey"), p("hello"))
    let left_tagged = tagged_doc(vec![tag("a"), tagged_paragraph_text("hello").into()]);
    let right = doc(vec![paragraph_text("hey"), paragraph_text("hello")]);
    assert_diff_end_at_tag(left_tagged, right);
}
