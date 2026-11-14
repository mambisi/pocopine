//! A structural edit in a long document steps only what it changes.
//!
//! `split`, `join` and `lift` build the new document whole; the step they
//! record must still be the smallest replace between the two documents, as
//! ProseMirror's are. A whole-document step made every consumer of the step
//! (history, plugins, the view) pay for the whole document on each Enter.

mod support;

use pine_richtext::commands::{join_backward, split_block};
use pine_richtext::history::{history_plugin, undo};
use pine_richtext::state::{EditorState, EditorStateConfig, Selection};
use pine_richtext::transform::Step;

use support::*;

fn long_state(caret: usize) -> EditorState {
    let blocks = (0..50)
        .map(|k| paragraph_text(&format!("Run {k}: twelve tests.")))
        .collect();
    EditorState::create(
        EditorStateConfig::new(pine_richtext::schema_basic::schema(), doc(blocks))
            .plugins(vec![history_plugin()])
            .selection(Selection::text(caret)),
    )
    .unwrap()
}

/// The position inside paragraph `k`, `offset` characters in.
fn inside(state: &EditorState, k: usize, offset: usize) -> usize {
    let before: usize = state
        .doc()
        .content()
        .iter()
        .take(k)
        .map(|n| n.node_size())
        .sum();
    before + 1 + offset
}

fn only_replace(steps: &[Step]) -> (usize, usize, usize) {
    assert_eq!(steps.len(), 1, "one step");
    let Step::Replace(step) = &steps[0] else {
        panic!("a replace step: {:?}", steps[0])
    };
    (step.from, step.to, step.slice.size())
}

#[test]
fn enter_in_the_middle_of_a_long_document_steps_the_split_alone() {
    let state = long_state(0);
    let at = inside(&state, 25, 4);
    let state = state
        .apply({
            let mut tr = state.tr();
            tr.set_selection(Selection::text(at)).unwrap();
            tr
        })
        .unwrap();
    let tr = split_block().apply(&state).expect("Enter splits");
    let (from, to, size) = only_replace(tr.transform().steps());
    assert!(
        from >= inside(&state, 25, 0) - 1 && to <= inside(&state, 26, 0),
        "{from}..{to}"
    );
    assert!(
        size <= 4,
        "the slice is the split, not the document: {size}"
    );
    let after = state.apply(tr).unwrap();
    assert_eq!(after.doc().child_count(), 51);
    // Undo puts the document back.
    let back = after.apply(undo().apply(&after).expect("undo")).unwrap();
    assert_eq!(back.doc(), state.doc());
}

#[test]
fn backspace_joining_two_blocks_steps_the_join_alone() {
    let state = long_state(0);
    let at = inside(&state, 30, 0);
    let state = state
        .apply({
            let mut tr = state.tr();
            tr.set_selection(Selection::text(at)).unwrap();
            tr
        })
        .unwrap();
    let tr = join_backward().apply(&state).expect("Backspace joins");
    let (from, to, size) = only_replace(tr.transform().steps());
    assert!(to - from <= 4 && size <= 4, "{from}..{to}, slice {size}");
    let after = state.apply(tr).unwrap();
    assert_eq!(after.doc().child_count(), 49);
}
