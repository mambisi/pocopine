//! Parity tests for `prosemirror-history`.
//!
//! Pine's history plugin keeps a `done` and `undone` branch of inverted
//! steps. Each non-history transaction lands on `done`; undo pops from
//! `done`, applies the inverted steps, and pushes the inverse onto
//! `undone`. Redo is symmetric.

mod support;

use pine_richtext::history::{
    HISTORY_COMMIT_MS_META, history_plugin, redo, redo_depth, undo, undo_depth,
};
use pine_richtext::state::{EditorState, EditorStateConfig, Selection};
use serde_json::json;

use support::*;

fn state_with_history(doc_node: pine_richtext::model::Node) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(pine_richtext::schema_basic::schema(), doc_node)
            .plugins(vec![history_plugin()]),
    )
    .unwrap()
}

#[test]
fn history_records_steps_and_undoes_to_original() {
    let state = state_with_history(doc(vec![paragraph_text("foo")]));
    assert_eq!(undo_depth(&state), 0);

    // Make an edit: insert "X" at position 2.
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    tr.insert_text("X").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("fXoo")]));
    assert_eq!(undo_depth(&state), 1);
    assert_eq!(redo_depth(&state), 0);

    // Undo restores the original.
    let tr = undo().apply(&state).expect("undo applies");
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foo")]));
    assert_eq!(undo_depth(&state), 0);
    assert_eq!(redo_depth(&state), 1);

    // Redo brings it back.
    let tr = redo().apply(&state).expect("redo applies");
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("fXoo")]));
    assert_eq!(undo_depth(&state), 1);
    assert_eq!(redo_depth(&state), 0);
}

#[test]
fn history_undo_with_empty_stack_returns_none() {
    let state = state_with_history(doc(vec![paragraph_text("foo")]));
    assert!(undo().apply(&state).is_none());
    assert!(redo().apply(&state).is_none());
}

#[test]
fn history_new_edit_clears_redo_branch() {
    let state = state_with_history(doc(vec![paragraph_text("foo")]));

    // Edit 1.
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    tr.insert_text("X").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(undo_depth(&state), 1);

    // Undo.
    let tr = undo().apply(&state).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(undo_depth(&state), 0);
    assert_eq!(redo_depth(&state), 1);

    // New edit.
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    tr.insert_text("Y").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("fYoo")]));
    assert_eq!(undo_depth(&state), 1);
    // Redo branch should be cleared.
    assert_eq!(redo_depth(&state), 0);
}

#[test]
fn history_undo_restores_prior_selection() {
    // Selection-before-edit is what undo restores — matches PM's behavior
    // (selection bookmark is captured from `old_state.selection()` when
    // the original transaction lands).
    let state = state_with_history(doc(vec![paragraph_text("foo")]));

    // Position the selection at 2, then make a single transaction that
    // moves it to 3 AND inserts "X".
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    let state = state.apply(tr).unwrap();

    let mut tr = state.tr();
    tr.set_selection(Selection::text(3)).unwrap();
    tr.insert_text("X").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foXo")]));

    // Undo restores selection to 2 — where the cursor was when this
    // transaction began.
    let tr = undo().apply(&state).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foo")]));
    match state.selection() {
        Selection::Text { anchor, head } => {
            assert_eq!(*anchor, 2);
            assert_eq!(*head, 2);
        }
        other => panic!("expected text selection after undo, got {other:?}"),
    }
}

#[test]
fn history_two_events_undo_in_order() {
    let state = state_with_history(doc(vec![paragraph_text("foo")]));

    // Edit 1: insert "X" at pos 2.
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    tr.insert_text("X").unwrap();
    let state = state.apply(tr).unwrap();

    // Edit 2: insert "Y" inside the paragraph at the end of "fXoo"
    // (pos 5 = paragraph-content offset 4 + 1 for the paragraph open).
    let mut tr = state.tr();
    tr.set_selection(Selection::text(5)).unwrap();
    tr.insert_text("Y").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("fXooY")]));
    assert_eq!(undo_depth(&state), 2);

    // Undo the Y first.
    let tr = undo().apply(&state).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("fXoo")]));

    // Then undo the X.
    let tr = undo().apply(&state).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("foo")]));
    assert_eq!(undo_depth(&state), 0);
    assert_eq!(redo_depth(&state), 2);
}

/// Helper: insert one character at `pos` with the given commit
/// timestamp injected on the transaction.
fn type_char_at(state: EditorState, pos: usize, ch: &str, ms: u64) -> EditorState {
    let mut tr = state.tr();
    tr.set_selection(Selection::text(pos)).unwrap();
    tr.insert_text(ch).unwrap();
    tr.set_meta(HISTORY_COMMIT_MS_META, json!(ms));
    state.apply(tr).unwrap()
}

#[test]
fn close_in_time_typing_merges_into_one_undo_step() {
    // PM's `newGroupDelay` shape: when the user types
    // continuously, undo should unwind in chunks, not one
    // character at a time. Three characters all inside the
    // merge window (`MERGE_WINDOW_MS = 500`) should land as
    // one undo unit.
    let state = state_with_history(doc(vec![paragraph_text("")]));
    assert_eq!(undo_depth(&state), 0);

    let state = type_char_at(state, 1, "h", 100);
    let state = type_char_at(state, 2, "i", 200);
    let state = type_char_at(state, 3, "!", 300);

    assert_eq!(state.doc(), &doc(vec![paragraph_text("hi!")]));
    assert_eq!(
        undo_depth(&state),
        1,
        "three close-in-time inserts merge to one undo step"
    );

    // One undo unwinds the whole burst.
    let tr = undo().apply(&state).expect("undo applies");
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("")]));
    assert_eq!(undo_depth(&state), 0);
    assert_eq!(redo_depth(&state), 1);
}

#[test]
fn typing_separated_by_pause_lands_as_two_undo_steps() {
    let state = state_with_history(doc(vec![paragraph_text("")]));
    // Two bursts, separated by > MERGE_WINDOW_MS.
    let state = type_char_at(state, 1, "a", 100);
    let state = type_char_at(state, 2, "b", 200);
    let state = type_char_at(state, 3, "c", 900); // > 500ms after "b"
    let state = type_char_at(state, 4, "d", 1000);

    assert_eq!(state.doc(), &doc(vec![paragraph_text("abcd")]));
    assert_eq!(
        undo_depth(&state),
        2,
        "two bursts separated by > MERGE_WINDOW_MS produce two undo units"
    );

    // First undo unwinds the second burst.
    let tr = undo().apply(&state).expect("undo applies");
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("ab")]));
    assert_eq!(undo_depth(&state), 1);

    // Second undo unwinds the first burst.
    let tr = undo().apply(&state).expect("undo applies");
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc(), &doc(vec![paragraph_text("")]));
    assert_eq!(undo_depth(&state), 0);
}

#[test]
fn transactions_without_commit_ms_meta_never_merge() {
    // Hosts that don't plug in a clock (or non-typing edits
    // that don't set commit_ms) keep the old "one event per
    // transaction" behaviour. No silent merging without a
    // declared timestamp.
    let state = state_with_history(doc(vec![paragraph_text("")]));
    let mut tr = state.tr();
    tr.set_selection(Selection::text(1)).unwrap();
    tr.insert_text("x").unwrap();
    let state = state.apply(tr).unwrap();
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    tr.insert_text("y").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(undo_depth(&state), 2);
}

#[test]
fn history_depth_caps_done_branch_at_pm_default() {
    // PM caps at 100 events. After 105 small typing
    // transactions, only the last 100 should be on the done
    // branch. Each transaction uses a non-merging
    // `commit_ms` (gap > MERGE_WINDOW_MS) so we get 105
    // distinct events to drive the cap.
    let mut state = state_with_history(doc(vec![paragraph_text("")]));
    for i in 0..105_u64 {
        state = type_char_at(state, 1 + (i as usize), "x", i.saturating_mul(1000));
    }
    assert_eq!(
        undo_depth(&state),
        100,
        "history depth cap drops oldest entries when the budget is exceeded"
    );
    // Doc still has all 105 chars — only the undo budget was
    // trimmed, not the rendered state.
    assert_eq!(state.doc().text_content().len(), 105);
}
