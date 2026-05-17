//! Parity tests for `prosemirror-history`.
//!
//! Pine's history plugin keeps a `done` and `undone` branch of inverted
//! steps. Each non-history transaction lands on `done`; undo pops from
//! `done`, applies the inverted steps, and pushes the inverse onto
//! `undone`. Redo is symmetric.

mod support;

use pine_richtext::commands::Command;
use pine_richtext::history::{history_plugin, redo, redo_depth, undo, undo_depth};
use pine_richtext::state::{EditorState, EditorStateConfig, Selection};

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
