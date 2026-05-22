//! Parity tests for `prosemirror-commands/test/test-commands.ts`.
//!
//! Each upstream `describe()` block maps to a `#[test]` (or a small group
//! of `#[test]`s) here. Commands take a state and either produce a
//! `Transaction` or `None`; the test pattern is:
//!
//! ```rust,ignore
//! let state = state_with_doc(doc(...));
//! let tr = commands::some_command().apply(&state).expect("command applies");
//! let state = state.apply(tr).unwrap();
//! assert_eq!(state.doc(), &doc(...));
//! ```

mod support;

use pine_richtext::commands::{
    self, chain_commands, delete_selection, join_backward, join_down, join_forward,
    join_textblock_backward, join_textblock_forward, join_up, lift, lift_empty_block, select_all,
    select_node_backward, select_node_forward, select_parent_node, select_textblock_end,
    select_textblock_start, set_block_type, split_block, split_list_item, toggle_mark, wrap_in,
    wrap_in_list, Command,
};
use pine_richtext::model::Attrs;
use pine_richtext::schema_basic;
use pine_richtext::state::Selection;

use support::*;

fn run(
    cmd: &dyn Command,
    state: pine_richtext::state::EditorState,
) -> pine_richtext::state::EditorState {
    let tr = cmd.apply(&state).expect("command should apply");
    state.apply(tr).expect("transaction applies cleanly")
}

#[test]
fn commands_delete_selection_clears_text_range() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("fo<a>oba<b>r").into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*delete_selection(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("for")]));
}

#[test]
fn commands_delete_selection_no_op_when_empty() {
    let state = state_with_doc(doc(vec![paragraph_text("foo")]));
    assert!(
        delete_selection().apply(&state).is_none(),
        "delete_selection should not apply when selection is empty",
    );
}

#[test]
fn commands_select_all_targets_the_whole_doc() {
    let state = state_with_doc(doc(vec![paragraph_text("foo"), paragraph_text("bar")]));
    let state = run(&*select_all(), state);
    assert!(matches!(state.selection(), Selection::All));
}

#[test]
fn commands_select_all_no_op_when_already_all() {
    let state = state_with_doc(doc(vec![paragraph_text("foo")]));
    let mut tr = state.tr();
    tr.set_selection(Selection::All).unwrap();
    let state = state.apply(tr).unwrap();
    assert!(
        select_all().apply(&state).is_none(),
        "select_all should not apply when AllSelection is already active",
    );
}

#[test]
fn commands_select_parent_node_walks_outward_one_level() {
    // doc(blockquote(p("foo<a>bar")))
    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_paragraph_text(
        "foo<a>bar",
    )
    .into()])
    .into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*select_parent_node(), state);
    // The selection should now wrap the paragraph (the cursor's parent).
    match state.selection() {
        Selection::Node { anchor } => assert_eq!(*anchor, 1),
        other => panic!("expected node selection of the paragraph, got {other:?}"),
    }
}

#[test]
fn commands_select_textblock_start_moves_cursor_to_start() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("hel<a>lo").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*select_textblock_start(), state);
    // Paragraph content starts at position 1 (after the open token).
    match state.selection() {
        Selection::Text { anchor, head } => {
            assert_eq!(*anchor, 1);
            assert_eq!(*head, 1);
        }
        other => panic!("expected text selection at textblock start, got {other:?}"),
    }
}

#[test]
fn commands_select_textblock_end_moves_cursor_to_end() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("hel<a>lo").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*select_textblock_end(), state);
    // p("hello") content ends at content_size 5; doc position 1 + 5 = 6.
    match state.selection() {
        Selection::Text { anchor, head } => {
            assert_eq!(*anchor, 6);
            assert_eq!(*head, 6);
        }
        other => panic!("expected text selection at textblock end, got {other:?}"),
    }
}

#[test]
fn commands_chain_falls_through_until_one_applies() {
    // First command never applies; second always applies; third would also
    // apply but should be skipped because chain returns the first match.
    let state = state_with_doc(doc(vec![paragraph_text("foo")]));
    let chain = chain_commands(vec![
        // delete_selection — selection is empty, won't apply.
        delete_selection(),
        // select_all — always applies (unless already All).
        select_all(),
        // Sentinel: a closure that panics if called.
        Box::new(|_state: &pine_richtext::state::EditorState| -> Option<pine_richtext::state::Transaction> {
            panic!("chain should have returned before reaching the sentinel");
        }),
    ]);
    let state = run(&*chain, state);
    assert!(matches!(state.selection(), Selection::All));
}

// ---------- lift / join / split ----------

#[test]
fn commands_lift_paragraph_out_of_blockquote() {
    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_paragraph_text(
        "fo<a>o",
    )
    .into()])
    .into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*lift(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foo")]));
}

#[test]
fn commands_lift_does_not_apply_when_already_at_top_level() {
    let state = state_with_doc(doc(vec![paragraph_text("foo")]));
    assert!(lift().apply(&state).is_none());
}

#[test]
fn commands_lift_empty_block_lifts_only_child_from_blockquote() {
    // Mirrors upstream's "lifts an only child":
    //   doc(blockquote(p("foo")), blockquote(p(<a>)))
    //   → doc(blockquote(p("foo")), p())
    let tagged = tagged_doc(vec![
        tagged_blockquote(vec![tagged_paragraph_text("foo").into()]).into(),
        tagged_blockquote(vec![tagged_paragraph(vec![tag("a")]).into()]).into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*lift_empty_block(), state);
    assert_eq!(
        state.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![paragraph_text("foo")]).unwrap(),
            empty_paragraph(),
        ]),
    );
}

#[test]
fn commands_lift_empty_block_no_op_in_non_empty_paragraph() {
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("foo").into(),
    ])
    .into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    assert!(lift_empty_block().apply(&state).is_none());
}

#[test]
fn commands_join_up_merges_two_paragraphs() {
    // doc(p("foo"), p("ba<a>r")) — join_up joins the second para into the first.
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tagged_paragraph_text("ba<a>r").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_up(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_join_up_does_not_apply_in_first_block() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("f<a>oo").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();
    assert!(join_up().apply(&state).is_none());
}

#[test]
fn commands_join_down_merges_with_next_paragraph() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("fo<a>o").into(),
        tagged_paragraph_text("bar").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_down(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_split_block_splits_at_cursor() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("fo<a>o").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*split_block(), state);
    assert_eq!(
        state.doc(),
        &doc(vec![paragraph_text("fo"), paragraph_text("o")]),
    );
    assert_eq!(state.selection(), &Selection::text(5));
}

#[test]
fn commands_split_block_at_heading_end_creates_default_paragraph() {
    let tagged = tagged_doc(vec![tagged_heading_text(1, "Head<a>").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*split_block(), state);
    assert_eq!(
        state.doc(),
        &doc(vec![heading(1, "Head"), empty_paragraph()])
    );
    assert_eq!(state.selection(), &Selection::text(7));
}

#[test]
fn commands_split_block_inside_heading_keeps_heading_type() {
    let tagged = tagged_doc(vec![tagged_heading_text(1, "He<a>ad").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*split_block(), state);
    assert_eq!(state.doc(), &doc(vec![heading(1, "He"), heading(1, "ad")]));
    assert_eq!(state.selection(), &Selection::text(5));
}

#[test]
fn commands_split_block_deletes_then_splits_a_range() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("fo<a>oba<b>r").into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*split_block(), state);
    assert_eq!(
        state.doc(),
        &doc(vec![paragraph_text("fo"), paragraph_text("r")]),
    );
    assert_eq!(state.selection(), &Selection::text(5));
}

#[test]
fn commands_split_list_item_keeps_selection_in_new_item_for_typing() {
    let tagged = tagged_doc(vec![tagged_bullet_list(vec![tagged_list_item_text(
        "one<a>",
    )
    .into()])
    .into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*split_list_item(&["list_item", "task_item"]), state);
    assert_eq!(
        state.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("one"),
            list_item(vec![empty_paragraph()])
        ])]),
    );
    assert_eq!(state.selection(), &Selection::text(10));

    let mut tr = state.tr();
    tr.insert_text("x").unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(
        state.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("one"),
            list_item_text("x")
        ])]),
    );
    assert_eq!(state.selection(), &Selection::text(11));
}

// ---------- wrap_in / set_block_type / toggle_mark ----------

#[test]
fn commands_wrap_in_wraps_paragraph_in_blockquote() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("fo<a>o").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*wrap_in("blockquote", Attrs::new()), state);
    assert_eq!(
        state.doc(),
        &doc(vec![
            schema_basic::blockquote(vec![paragraph_text("foo")]).unwrap()
        ]),
    );
}

#[test]
fn commands_wrap_in_does_not_apply_when_wrapper_cannot_contain_target() {
    // Wrapping a doc in a paragraph is invalid (paragraph holds inline only).
    let state = state_with_doc(doc(vec![paragraph_text("foo")]));
    let mut tr = state.tr();
    tr.set_selection(Selection::All).unwrap();
    let state = state.apply(tr).unwrap();
    assert!(wrap_in("paragraph", Attrs::new()).apply(&state).is_none());
}

#[test]
fn commands_wrap_in_list_creates_one_item_per_selected_block() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("al<a>pha").into(),
        tagged_paragraph_text("bra<b>vo").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(
        &*wrap_in_list("bullet_list", "list_item", Attrs::new()),
        state,
    );
    assert_eq!(
        state.doc(),
        &doc(vec![bullet_list(vec![
            list_item_text("alpha"),
            list_item_text("bravo"),
        ])]),
    );
}

#[test]
fn commands_set_block_type_converts_paragraph_to_heading() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("hel<a>lo").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), serde_json::json!(2));
    let state = run(&*set_block_type("heading", attrs), state);
    assert_eq!(state.doc(), &doc(vec![heading(2, "hello")]));
}

#[test]
fn commands_toggle_mark_adds_mark_when_absent_and_removes_when_present() {
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>bar<b>").into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    let state = state.apply(tr).unwrap();

    // First toggle adds.
    let state = run(&*toggle_mark(em.clone()), state);
    assert_eq!(
        state.doc(),
        &doc(vec![paragraph(vec![
            text("foo"),
            marked_text("bar", vec![em.clone()]),
        ])]),
    );

    // Re-select the same range, second toggle removes.
    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    let state = state.apply(tr).unwrap();
    let state = run(&*toggle_mark(em), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_toggle_mark_on_empty_selection_updates_stored_marks() {
    let em = schema_basic::em().unwrap();
    let state = state_with_doc(doc(vec![paragraph_text("foo")]));
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*toggle_mark(em.clone()), state);
    let marks: Vec<&str> = state
        .stored_marks()
        .map(|s| s.iter().map(|m| m.type_name()).collect())
        .unwrap_or_default();
    assert_eq!(marks, ["em"]);
}

// ---------- Wave 3: backward/forward join + select-node ----------

#[test]
fn commands_join_textblock_backward_joins_adjacent_paragraphs() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tagged_paragraph_text("<a>bar").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_textblock_backward(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_join_textblock_backward_no_op_when_not_at_start() {
    let tagged = tagged_doc(vec![tagged_paragraph_text("foo<a>").into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();
    assert!(join_textblock_backward().apply(&state).is_none());
}

#[test]
fn commands_join_textblock_forward_joins_with_next_paragraph() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo<a>").into(),
        tagged_paragraph_text("bar").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_textblock_forward(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_join_backward_lifts_when_no_sibling_before() {
    // Cursor at start of a single paragraph inside a blockquote — no
    // sibling before to join with, so join_backward falls back to lift.
    let tagged = tagged_doc(vec![tagged_blockquote(vec![tagged_paragraph(vec![
        tag("a"),
        tagged_text("foo").into(),
    ])
    .into()])
    .into()]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_backward(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foo")]));
}

#[test]
fn commands_join_backward_joins_with_previous_paragraph() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tagged_paragraph_text("<a>bar").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_backward(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_join_backward_deletes_atom_before_cursor() {
    // doc(p("foo"), hr, p("<a>bar")) → Backspace at start of "bar" deletes the hr.
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tagged_horizontal_rule().into(),
        tagged_paragraph_text("<a>bar").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_backward(), state);
    assert_eq!(
        state.doc(),
        &doc(vec![paragraph_text("foo"), paragraph_text("bar")]),
    );
}

#[test]
fn commands_join_forward_joins_with_next_paragraph() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo<a>").into(),
        tagged_paragraph_text("bar").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*join_forward(), state);
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foobar")]));
}

#[test]
fn commands_select_node_backward_selects_preceding_atom() {
    let tagged = tagged_doc(vec![
        tagged_horizontal_rule().into(),
        tagged_paragraph_text("<a>foo").into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*select_node_backward(), state);
    match state.selection() {
        Selection::Node { anchor } => assert_eq!(*anchor, 0),
        other => panic!("expected node selection of hr at pos 0, got {other:?}"),
    }
}

#[test]
fn commands_select_node_forward_selects_following_atom() {
    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo<a>").into(),
        tagged_horizontal_rule().into(),
    ]);
    let pos = tagged.tag("a");
    let state = state_with_doc(tagged.node);
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    let state = state.apply(tr).unwrap();

    let state = run(&*select_node_forward(), state);
    match state.selection() {
        Selection::Node { anchor } => assert_eq!(*anchor, 5), // after p("foo")
        other => panic!("expected node selection of hr after p, got {other:?}"),
    }
}

// Silence the `use commands::self` warning if it ever shows up — keeps the
// import block tidy for the next wave.
#[allow(unused_imports)]
use commands as _;
