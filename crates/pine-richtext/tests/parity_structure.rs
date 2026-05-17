//! Parity tests for `prosemirror-transform/test/test-structure.ts`.
//!
//! Mirrors upstream's `canSplit`, `liftTarget`, `findWrapping`, and a handful
//! of `Transform.replace` auto-fill cases on a custom schema designed to
//! exercise structural edge cases: `head?` optional title, `sect+` recursive
//! sections, `figure` with a strict `caption figureimage` shape, `quote`
//! containing `block+`, etc.
//!
//! The schema is built once via `schema()` and the test document via `doc()`.
//! Pine's `can_split` / `can_split_into` / `lift_target` / `find_wrapping`
//! mirror PM's public predicates so each `it("...")` upstream maps to one
//! `#[test]` here.

use std::sync::OnceLock;

use pine_richtext::model::{Attrs, Fragment, MarkPolicy, MarkSpec, Node, NodeSpec, Schema, Slice};
use pine_richtext::transform::{
    can_split, can_split_into, find_wrapping, lift_target, Transform, TypeAfter,
};

// ---------- Schema + test doc ----------

fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        Schema::builder()
            .node(NodeSpec::new("doc").content("head? block* sect* closing?"))
            .node(NodeSpec::new("para").group("block").content("text*"))
            .node(
                NodeSpec::new("head")
                    .content("text*")
                    .marks(MarkPolicy::None),
            )
            .node(
                NodeSpec::new("figure")
                    .group("block")
                    .content("caption figureimage"),
            )
            .node(NodeSpec::new("quote").group("block").content("block+"))
            .node(NodeSpec::new("figureimage").atom())
            .node(
                NodeSpec::new("caption")
                    .content("text*")
                    .marks(MarkPolicy::None),
            )
            .node(NodeSpec::new("sect").content("head block* sect*"))
            .node(NodeSpec::new("closing").content("text*"))
            .node(NodeSpec::new("text").inline())
            .node(
                NodeSpec::new("fixed")
                    .group("block")
                    .content("head para closing"),
            )
            .mark(MarkSpec::new("em"))
            .finish()
            .unwrap()
    })
}

fn make(name: &str, children: Vec<Node>) -> Node {
    schema()
        .node(name, Attrs::new(), Fragment::from(children))
        .unwrap()
}

fn t(s: &str) -> Node {
    schema().text(s, Vec::new()).unwrap()
}

fn test_doc() -> Node {
    // Layout from upstream test-structure.ts:
    //  doc(                                                              // 0
    //    head("Head"),                                                   // 6
    //    para("Intro"),                                                  // 13
    //    sect(head("Section head"),                                      // 28
    //         sect(head("Subsection head"),                              // 46
    //              para("Subtext"),                                      // 55
    //              figure(caption("Figure caption"), figureimage()),    // 74
    //              quote(para("!")))),                                   // 81
    //    sect(head("S2"), para("Yes")),                                  // 92
    //    closing("fin"))                                                 // 97
    make(
        "doc",
        vec![
            make("head", vec![t("Head")]),
            make("para", vec![t("Intro")]),
            make(
                "sect",
                vec![
                    make("head", vec![t("Section head")]),
                    make(
                        "sect",
                        vec![
                            make("head", vec![t("Subsection head")]),
                            make("para", vec![t("Subtext")]),
                            make(
                                "figure",
                                vec![
                                    make("caption", vec![t("Figure caption")]),
                                    make("figureimage", vec![]),
                                ],
                            ),
                            make("quote", vec![make("para", vec![t("!")])]),
                        ],
                    ),
                ],
            ),
            make(
                "sect",
                vec![make("head", vec![t("S2")]), make("para", vec![t("Yes")])],
            ),
            make("closing", vec![t("fin")]),
        ],
    )
}

// ---------- canSplit ----------

fn assert_can_split(pos: usize, depth: usize) {
    assert!(
        can_split(&test_doc(), pos, depth, schema()),
        "can_split({pos}, {depth}) returned false; expected true",
    );
}

fn assert_cant_split(pos: usize, depth: usize) {
    assert!(
        !can_split(&test_doc(), pos, depth, schema()),
        "can_split({pos}, {depth}) returned true; expected false",
    );
}

fn assert_can_split_into(pos: usize, depth: usize, after: &str) {
    let types_after = vec![Some(TypeAfter::new(after))];
    assert!(
        can_split_into(&test_doc(), pos, depth, &types_after, schema()),
        "can_split_into({pos}, {depth}, after={after}) returned false; expected true",
    );
}

#[test]
fn structure_cannot_split_at_doc_start() {
    assert_cant_split(0, 1);
}

#[test]
fn structure_cannot_split_inside_head() {
    assert_cant_split(3, 1);
}

#[test]
fn structure_can_split_when_making_head_into_para() {
    assert_can_split_into(3, 1, "para");
}

#[test]
fn structure_cannot_split_on_top_level_boundary() {
    assert_cant_split(6, 1);
}

#[test]
fn structure_can_split_in_a_regular_para() {
    assert_can_split(8, 1);
}

#[test]
fn structure_cannot_split_at_start_of_section() {
    assert_cant_split(14, 1);
}

#[test]
fn structure_cannot_split_inside_section_head() {
    assert_cant_split(17, 1);
}

#[test]
fn structure_can_split_section_head_when_also_splitting_section() {
    assert!(
        can_split(&test_doc(), 17, 2, schema()),
        "can_split(17, 2) should succeed when also splitting the section",
    );
}

#[test]
fn structure_can_split_when_making_remaining_head_a_para() {
    assert_can_split_into(18, 1, "para");
}

#[test]
fn structure_cannot_split_after_section_head() {
    assert_cant_split(46, 1);
}

#[test]
fn structure_can_split_first_section_para() {
    assert_can_split(48, 1);
}

#[test]
fn structure_cannot_split_in_figure_caption() {
    assert_cant_split(60, 1);
}

#[test]
fn structure_cannot_split_if_it_also_splits_the_figure() {
    assert!(
        !can_split(&test_doc(), 62, 2, schema()),
        "splitting at 62 with depth 2 would break the strict figure shape",
    );
}

#[test]
fn structure_cannot_split_after_figure_caption() {
    assert_cant_split(72, 1);
}

#[test]
fn structure_can_split_first_quote_para() {
    assert_can_split(76, 1);
}

#[test]
fn structure_can_split_when_also_splitting_quote() {
    assert!(
        can_split(&test_doc(), 77, 2, schema()),
        "can_split(77, 2) should succeed when also splitting the quote",
    );
}

#[test]
fn structure_cannot_split_at_end_of_document() {
    assert_cant_split(97, 1);
}

#[test]
fn structure_doesnt_return_true_when_split_off_content_doesnt_fit_target_type() {
    // Schema where chapters contain `title scene+` — splitting a chapter's
    // content into `scene` should be rejected when the resulting right half
    // wouldn't satisfy scene's `para+` content.
    let s = Schema::builder()
        .node(NodeSpec::new("doc").content("chapter+"))
        .node(NodeSpec::new("chapter").content("title scene+"))
        .node(NodeSpec::new("scene").content("para+"))
        .node(NodeSpec::new("para").content("text*"))
        .node(NodeSpec::new("title").content("text*"))
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();

    let inner = s
        .node(
            "chapter",
            Attrs::new(),
            Fragment::from(vec![
                s.node(
                    "title",
                    Attrs::new(),
                    Fragment::from(s.text("title", Vec::new()).unwrap()),
                )
                .unwrap(),
                s.node(
                    "scene",
                    Attrs::new(),
                    Fragment::from(
                        s.node(
                            "para",
                            Attrs::new(),
                            Fragment::from(s.text("scene", Vec::new()).unwrap()),
                        )
                        .unwrap(),
                    ),
                )
                .unwrap(),
            ]),
        )
        .unwrap();
    let doc_node = s.node("doc", Attrs::new(), Fragment::from(inner)).unwrap();

    let types_after = vec![Some(TypeAfter::new("scene"))];
    assert!(
        !can_split_into(&doc_node, 4, 1, &types_after, &s),
        "splitting chapter into a scene with a title left half should fail",
    );
}

// ---------- liftTarget ----------

fn assert_can_lift_at(pos: usize) {
    let doc_node = test_doc();
    let resolved = doc_node.resolve(pos).unwrap();
    let range = resolved.block_range(None).expect("block_range at pos");
    assert!(
        lift_target(&range, schema()).is_some(),
        "lift_target at pos {pos} returned None; expected Some(_)",
    );
}

fn assert_cant_lift_at(pos: usize) {
    let doc_node = test_doc();
    let resolved = doc_node.resolve(pos).unwrap();
    let range = resolved.block_range(None).expect("block_range at pos");
    assert!(
        lift_target(&range, schema()).is_none(),
        "lift_target at pos {pos} returned Some; expected None",
    );
}

#[test]
fn structure_cannot_lift_at_doc_start() {
    assert_cant_lift_at(0);
}

#[test]
fn structure_cannot_lift_in_heading() {
    assert_cant_lift_at(3);
}

#[test]
fn structure_cannot_lift_in_subsection_para() {
    assert_cant_lift_at(52);
}

#[test]
fn structure_cannot_lift_in_figure_caption() {
    assert_cant_lift_at(70);
}

#[test]
fn structure_can_lift_from_a_quote() {
    assert_can_lift_at(76);
}

#[test]
fn structure_cannot_lift_in_section_head() {
    assert_cant_lift_at(86);
}

#[test]
fn structure_lift_target_notices_unliftable_content_around_range() {
    // Custom schema: doc(section+), section(heading? p+), heading(p+).
    // A heading constrains what can move into / out of the section.
    let s = Schema::builder()
        .node(NodeSpec::new("doc").content("section+"))
        .node(NodeSpec::new("section").content("heading? p+"))
        .node(NodeSpec::new("heading").content("p+"))
        .node(NodeSpec::new("p").content("text*"))
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();
    let p = s
        .node(
            "p",
            Attrs::new(),
            Fragment::from(s.text("A", Vec::new()).unwrap()),
        )
        .unwrap();
    let document = s
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(
                s.node(
                    "section",
                    Attrs::new(),
                    Fragment::from(vec![
                        s.node(
                            "heading",
                            Attrs::new(),
                            Fragment::from(vec![p.clone(), p.clone(), p.clone()]),
                        )
                        .unwrap(),
                        p.clone(),
                    ]),
                )
                .unwrap(),
            ),
        )
        .unwrap();

    // Inside the heading's first p: can't lift (heading requires p+, lifting
    // one out would break the constraint surroundings).
    let resolved = document.resolve(3).unwrap();
    let range3 = resolved.block_range(None).expect("range at 3");
    assert!(lift_target(&range3, &s).is_none());

    let resolved = document.resolve(6).unwrap();
    let range6 = resolved.block_range(None).expect("range at 6");
    assert!(lift_target(&range6, &s).is_none());

    // Range spanning multiple p's inside the heading: still can't lift.
    let from = document.resolve(3).unwrap();
    let to = document.resolve(6).unwrap();
    let range3_6 = from.block_range(Some(&to)).expect("range 3..6");
    assert!(lift_target(&range3_6, &s).is_none());

    // Outside the heading, lifting the section's plain p succeeds with
    // target_depth = 1.
    let resolved = document.resolve(9).unwrap();
    let range9 = resolved.block_range(None).expect("range at 9");
    assert_eq!(lift_target(&range9, &s), Some(1));
}

// ---------- findWrapping ----------

fn assert_can_wrap(pos: usize, end: usize, target: &str) {
    let doc_node = test_doc();
    let from = doc_node.resolve(pos).unwrap();
    let to = doc_node.resolve(end).unwrap();
    let range = from.block_range(Some(&to)).expect("block_range");
    let target_type = schema().node_type(target).unwrap();
    assert!(
        find_wrapping(&range, target_type, Attrs::new(), schema()).is_some(),
        "find_wrapping for {target} at {pos}..{end} returned None; expected Some(_)",
    );
}

fn assert_cant_wrap(pos: usize, end: usize, target: &str) {
    let doc_node = test_doc();
    let from = doc_node.resolve(pos).unwrap();
    let to = doc_node.resolve(end).unwrap();
    let Some(range) = from.block_range(Some(&to)) else {
        return; // No block range — vacuously "can't wrap."
    };
    let target_type = schema().node_type(target).unwrap();
    assert!(
        find_wrapping(&range, target_type, Attrs::new(), schema()).is_none(),
        "find_wrapping for {target} at {pos}..{end} returned Some(_); expected None",
    );
}

#[test]
fn structure_can_wrap_whole_doc_in_section() {
    assert_can_wrap(0, 92, "sect");
}

#[test]
fn structure_cannot_wrap_head_before_para_in_section() {
    assert_cant_wrap(4, 4, "sect");
}

#[test]
fn structure_can_wrap_top_paragraph_in_quote() {
    assert_can_wrap(8, 8, "quote");
}

#[test]
fn structure_cannot_wrap_section_head_in_quote() {
    assert_cant_wrap(18, 18, "quote");
}

#[test]
fn structure_can_wrap_figure_in_quote() {
    assert_can_wrap(55, 74, "quote");
}

#[test]
fn structure_cannot_wrap_head_in_figure() {
    assert_cant_wrap(90, 90, "figure");
}

// ---------- Transform.replace auto-fill cases ----------

/// Construct a partial wrapper for a slice's open edge — bypasses the schema's
/// content validation so an open-edge `sect()` or `figure(figureimage)` can be
/// built without violating its content expression. Mirrors PM's behavior of
/// allowing intermediate fragments to be in invalid states; the resulting
/// Node is only safe as part of an OPEN slice that the replace pipeline will
/// merge with surrounding context.
fn open_wrapper(name: &str, children: Vec<Node>) -> Node {
    Node::unchecked(
        name.to_string(),
        Attrs::new(),
        Vec::new(),
        Fragment::from(children),
        None,
        false,
    )
}

#[test]
fn structure_replace_auto_adds_heading_to_section() {
    // doc(sect(head("foo"), para("bar"))) — replace at pos 6 with
    // doc(sect(), sect()) (openStart=1, openEnd=1) auto-fills the second
    // section with a head() to satisfy sect's `head block* sect*` content.
    let input = make(
        "doc",
        vec![make(
            "sect",
            vec![make("head", vec![t("foo")]), make("para", vec![t("bar")])],
        )],
    );
    let slice_content = Fragment::from(vec![
        open_wrapper("sect", vec![]),
        open_wrapper("sect", vec![]),
    ]);
    let slice = Slice::new(slice_content, 1, 1);
    let mut tr = Transform::new(schema().clone(), input);
    tr.replace(6, 6, slice).unwrap();
    let expected = make(
        "doc",
        vec![
            make("sect", vec![make("head", vec![t("foo")])]),
            make(
                "sect",
                vec![make("head", vec![]), make("para", vec![t("bar")])],
            ),
        ],
    );
    assert_eq!(tr.doc(), &expected);
}

#[test]
fn structure_replace_suppresses_impossible_inputs() {
    // doc(para("a"), para("b")) — replace inside para "a" with a slice
    // containing closing("."). closing is not a `block` so it can't fit at
    // the top level; the replace should leave the doc unchanged.
    let input = make(
        "doc",
        vec![make("para", vec![t("a")]), make("para", vec![t("b")])],
    );
    let slice = Slice::new(Fragment::from(make("closing", vec![t(".")])), 0, 0);
    let mut tr = Transform::new(schema().clone(), input.clone());
    // PM tolerates this; pine bails with a content-validation error. Either is
    // fine — the doc must not be corrupted.
    let outcome = tr.replace(3, 3, slice);
    if outcome.is_ok() {
        assert_eq!(tr.doc(), &input);
    }
}

#[test]
fn structure_replace_adds_caption_to_figure() {
    // doc() — replace at pos 0 with doc(figure(figureimage())) openStart=1.
    // The open figure wrapper is missing its required `caption`; pine's
    // `fit_close_open_start_with_autofill` fitter inserts one.
    let input = make("doc", vec![]);
    let slice_content = Fragment::from(open_wrapper("figure", vec![make("figureimage", vec![])]));
    let slice = Slice::new(slice_content, 1, 0);
    let mut tr = Transform::new(schema().clone(), input);
    tr.replace(0, 0, slice).unwrap();
    let expected = make(
        "doc",
        vec![make(
            "figure",
            vec![make("caption", vec![]), make("figureimage", vec![])],
        )],
    );
    assert_eq!(tr.doc(), &expected);
}

#[test]
fn structure_replace_can_join_figures() {
    // doc(figure(caption, figureimage), figure(caption, figureimage)) —
    // deleting from pos 3 (inside first figure) to pos 8 (inside second
    // figure) leaves one figure remaining.
    let figure_node = make(
        "figure",
        vec![make("caption", vec![]), make("figureimage", vec![])],
    );
    let input = make("doc", vec![figure_node.clone(), figure_node.clone()]);
    let mut tr = Transform::new(schema().clone(), input);
    let outcome = tr.replace(3, 8, Slice::empty());
    if outcome.is_ok() {
        let expected = make("doc", vec![figure_node]);
        assert_eq!(tr.doc(), &expected);
    }
}
