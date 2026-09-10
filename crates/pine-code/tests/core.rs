use pine_code::*;

fn doc(text: &str) -> TextDocument {
    TextDocument::new(text, DocumentLimits::default()).unwrap()
}
fn editor(text: &str) -> Editor {
    Editor::new(text, DocumentLimits::default(), HistoryLimits::default()).unwrap()
}
fn replace(
    editor: &mut Editor,
    from: usize,
    to: usize,
    text: &str,
    time: u64,
    origin: EditOrigin,
) -> CodeChange {
    let changes = ChangeSet::new(
        editor.state().document(),
        vec![Change::replace(from, to, text)],
    )
    .unwrap();
    let mut transaction = editor.state().transaction(changes, origin);
    transaction.selection = Some(Selection::caret(from + normalize_lf(text).len()));
    editor.dispatch(transaction, time).unwrap()
}

#[test]
fn normalized_lines_preserve_blank_lines_tabs_and_final_newline() {
    let document = doc("\r\n\t猫 \r\n\r");
    assert_eq!(document.text(), "\n\t猫 \n\n");
    assert_eq!(document.line_count(), 4);
    assert_eq!(document.line(1), Some("\t猫 "));
    assert_eq!(document.line(3), Some(""));
    assert_eq!(document.export_text(LineEnding::CrLf), "\r\n\t猫 \r\n\r\n");
    assert_eq!(doc("").line_count(), 1);
}

#[test]
fn utf16_conversion_rejects_surrogate_interiors_and_invalid_bytes() {
    let text = "a😀e\u{301}";
    assert_eq!(byte_to_utf16(text, 5), Ok(3));
    assert_eq!(utf16_to_byte(text, 3), Ok(5));
    assert_eq!(utf16_to_byte(text, 2), Err(CodeError::InvalidPosition));
    assert_eq!(byte_to_utf16(text, 3), Err(CodeError::InvalidPosition));
    assert_eq!(utf16_to_byte(text, 99), Err(CodeError::InvalidPosition));
}

#[test]
fn unicode_replacements_match_an_independent_oracle_and_invert() {
    for source in [
        "",
        "abc",
        "😀a猫",
        "e\u{301}\n🙂\n",
        "👩\u{200d}💻אבג",
        "aaaaaa",
    ] {
        let original = doc(source);
        let boundaries: Vec<_> = source
            .char_indices()
            .map(|(i, _)| i)
            .chain([source.len()])
            .collect();
        for &from in &boundaries {
            for &to in boundaries.iter().filter(|&&to| to >= from) {
                for insert in ["", "X", "猫😀", "\n", "\r\n"] {
                    let changes =
                        ChangeSet::new(&original, vec![Change::replace(from, to, insert)]).unwrap();
                    let applied = changes.apply(&original, DocumentLimits::default()).unwrap();
                    assert_eq!(
                        applied.text(),
                        format!(
                            "{}{}{}",
                            &source[..from],
                            normalize_lf(insert),
                            &source[to..]
                        )
                    );
                    let inverse = changes
                        .inverse(&original, DocumentLimits::default())
                        .unwrap();
                    assert_eq!(
                        inverse.apply(&applied, DocumentLimits::default()).unwrap(),
                        original
                    );
                    for &position in &boundaries {
                        for bias in [Bias::Before, Bias::After] {
                            let mapped = changes
                                .map_offset(&original, TextOffset(position), bias)
                                .unwrap();
                            applied.check_offset(mapped).unwrap();
                            assert_eq!(
                                utf16_to_byte(
                                    applied.text(),
                                    byte_to_utf16(applied.text(), mapped.0).unwrap()
                                )
                                .unwrap(),
                                mapped.0
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn disjoint_changes_share_pre_edit_coordinates_and_adjacent_deletions_invert() {
    let original = doc("abcdef");
    let changes = ChangeSet::new(
        &original,
        vec![Change::replace(4, 6, "Z"), Change::replace(0, 1, "XY")],
    )
    .unwrap();
    let result = changes.apply(&original, DocumentLimits::default()).unwrap();
    assert_eq!(result.text(), "XYbcdZ");
    assert_eq!(
        changes
            .inverse(&original, DocumentLimits::default())
            .unwrap()
            .apply(&result, DocumentLimits::default())
            .unwrap(),
        original
    );
    let deletes = ChangeSet::new(
        &original,
        vec![Change::replace(0, 2, ""), Change::replace(2, 4, "")],
    )
    .unwrap();
    let result = deletes.apply(&original, DocumentLimits::default()).unwrap();
    assert_eq!(
        deletes
            .inverse(&original, DocumentLimits::default())
            .unwrap()
            .apply(&result, DocumentLimits::default())
            .unwrap(),
        original
    );
}

#[test]
fn selection_edges_exclude_adjacent_insertions_and_preserve_direction() {
    let original = doc("abcd");
    for position in [1, 3] {
        let changes =
            ChangeSet::new(&original, vec![Change::replace(position, position, "X")]).unwrap();
        let expected = if position == 1 {
            Selection::between(2, 4)
        } else {
            Selection::between(1, 3)
        };
        assert_eq!(
            changes
                .map_selection(&original, Selection::between(1, 3))
                .unwrap(),
            expected
        );
        assert_eq!(
            changes
                .map_selection(&original, Selection::between(3, 1))
                .unwrap(),
            Selection::between(expected.head.0, expected.anchor.0)
        );
    }
    let changes = ChangeSet::new(&original, vec![Change::replace(0, 4, "XYZ")]).unwrap();
    assert_eq!(
        changes
            .map_selection(&original, Selection::between(3, 1))
            .unwrap(),
        Selection::caret(3)
    );
}

#[test]
fn invalid_ranges_and_ambiguous_edits_are_rejected() {
    let original = doc("a😀b");
    assert!(ChangeSet::new(&original, vec![Change::replace(2, 3, "")]).is_err());
    assert!(ChangeSet::new(&original, vec![Change::replace(5, 1, "")]).is_err());
    assert_eq!(
        ChangeSet::new(
            &original,
            vec![Change::replace(1, 1, "A"), Change::replace(1, 1, "B")]
        ),
        Err(CodeError::OverlappingChanges)
    );
    assert_eq!(
        ChangeSet::new(
            &original,
            vec![Change::replace(0, 5, "A"), Change::replace(1, 6, "B")]
        ),
        Err(CodeError::OverlappingChanges)
    );
}

#[test]
fn invalid_selection_and_size_limits_leave_state_and_history_untouched() {
    let mut editor = Editor::new(
        "ab",
        DocumentLimits {
            max_bytes: 4,
            max_lines: 2,
        },
        HistoryLimits::default(),
    )
    .unwrap();
    let changes =
        ChangeSet::new(editor.state().document(), vec![Change::replace(0, 0, "x")]).unwrap();
    let mut transaction = editor.state().transaction(changes, EditOrigin::Api);
    transaction.selection = Some(Selection::caret(99));
    assert_eq!(
        editor.dispatch(transaction, 0).unwrap_err(),
        CodeError::InvalidPosition
    );
    for insert in ["12345", "\n\n"] {
        let changes = ChangeSet::new(
            editor.state().document(),
            vec![Change::replace(0, 0, insert)],
        )
        .unwrap();
        let transaction = editor.state().transaction(changes, EditOrigin::Api);
        assert_eq!(
            editor.dispatch(transaction, 0).unwrap_err(),
            CodeError::SizeLimit
        );
    }
    assert_eq!(editor.state().document().text(), "ab");
    assert_eq!(editor.state().revision(), DocumentRevision(0));
    assert!(!editor.can_undo());
}

#[test]
fn stale_in_bounds_selection_is_rejected_and_selection_does_not_advance_revision() {
    let mut editor = editor("abc");
    replace(&mut editor, 0, 0, "x", 0, EditOrigin::Api);
    assert!(matches!(
        editor.set_selection(Selection::between(1, 2), DocumentRevision(0)),
        Err(CodeError::StaleRevision { .. })
    ));
    editor
        .set_selection(Selection::between(2, 1), DocumentRevision(1))
        .unwrap();
    assert_eq!(editor.state().revision(), DocumentRevision(1));
    assert_eq!(editor.state().selection(), Selection::between(2, 1));
}

#[test]
fn typing_groups_undo_once_and_new_edits_invalidate_redo() {
    let mut editor = editor("");
    replace(&mut editor, 0, 0, "a", 100, EditOrigin::Input);
    replace(&mut editor, 1, 1, "😀", 200, EditOrigin::Input);
    replace(&mut editor, 5, 5, "b", 300, EditOrigin::Input);
    assert_eq!(editor.state().revision(), DocumentRevision(3));
    editor.undo().unwrap().unwrap();
    assert_eq!(editor.state().document().text(), "");
    assert_eq!(editor.state().revision(), DocumentRevision(4));
    assert_eq!(editor.state().selection(), Selection::caret(0));
    editor.redo().unwrap().unwrap();
    assert_eq!(editor.state().document().text(), "a😀b");
    assert_eq!(editor.state().selection(), Selection::caret(6));
    editor.undo().unwrap();
    replace(&mut editor, 0, 0, "new", 400, EditOrigin::Paste);
    assert!(!editor.can_redo());
}

#[test]
fn paste_and_composition_isolate_history_and_identical_load_resets_it() {
    let mut editor = editor("");
    replace(&mut editor, 0, 0, "a", 100, EditOrigin::Input);
    replace(&mut editor, 1, 1, "猫", 101, EditOrigin::Composition);
    editor.undo().unwrap();
    assert_eq!(editor.state().document().text(), "a");
    let revision = editor.state().revision();
    editor.load_document("a", revision).unwrap();
    assert_eq!(editor.state().revision(), DocumentRevision(revision.0 + 1));
    assert!(!editor.can_undo());
    assert!(!editor.can_redo());
}

#[test]
fn retention_evicts_whole_groups_and_applies_unretainable_edits() {
    let mut editor = Editor::new(
        "",
        DocumentLimits::default(),
        HistoryLimits {
            max_groups: 1,
            ..HistoryLimits::default()
        },
    )
    .unwrap();
    replace(&mut editor, 0, 0, "a", 0, EditOrigin::Api);
    replace(&mut editor, 1, 1, "b", 0, EditOrigin::Api);
    editor.undo().unwrap();
    assert_eq!(editor.state().document().text(), "a");
    assert!(!editor.can_undo());
    let mut editor = Editor::new(
        "",
        DocumentLimits::default(),
        HistoryLimits {
            max_bytes: 1,
            ..HistoryLimits::default()
        },
    )
    .unwrap();
    assert!(!replace(&mut editor, 0, 0, "still applies", 0, EditOrigin::Api).history_retained);
    assert_eq!(editor.state().document().text(), "still applies");
    assert!(!editor.can_undo());
}
