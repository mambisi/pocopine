use pine_code::{commands::*, search::*, *};

fn editor(text: &str) -> Editor {
    Editor::new(text, DocumentLimits::default(), HistoryLimits::default()).unwrap()
}
fn select(editor: &mut Editor, anchor: usize, head: usize) {
    editor
        .set_selection(Selection::between(anchor, head), editor.state().revision())
        .unwrap();
}

#[test]
fn enter_copies_only_indentation_before_the_caret() {
    for (text, at, expected) in [("    x", 2, "  \n    x"), ("\t  x", 4, "\t  x\n\t  ")] {
        let mut editor = editor(text);
        select(&mut editor, at, at);
        editor
            .dispatch(insert_newline(editor.state()).unwrap(), 0)
            .unwrap();
        assert_eq!(editor.state().document().text(), expected);
        editor.undo().unwrap();
        assert_eq!(editor.state().document().text(), text);
    }
}

#[test]
fn indentation_excludes_terminal_line_start_and_preserves_backwards_selection() {
    let mut editor = editor("a\nb\nc");
    select(&mut editor, 4, 0);
    editor
        .dispatch(indent_lines(editor.state(), "  ", false, 4).unwrap(), 0)
        .unwrap();
    assert_eq!(editor.state().document().text(), "  a\n  b\nc");
    assert_eq!(editor.state().selection(), Selection::between(8, 2));
    editor
        .dispatch(indent_lines(editor.state(), "  ", true, 4).unwrap(), 1)
        .unwrap();
    assert_eq!(editor.state().document().text(), "a\nb\nc");
    assert_eq!(editor.state().selection(), Selection::between(4, 0));
}

#[test]
fn indentation_stops_respect_existing_tabs_and_partial_space_units() {
    let mut editor = editor("\ta");
    select(&mut editor, 2, 2);
    editor
        .dispatch(indent_lines(editor.state(), "    ", false, 4).unwrap(), 0)
        .unwrap();
    assert_eq!(editor.state().document().text(), "\ta   ");
    let mut editor = self::editor("  a");
    select(&mut editor, 3, 3);
    editor
        .dispatch(indent_lines(editor.state(), "    ", true, 4).unwrap(), 0)
        .unwrap();
    assert_eq!(editor.state().document().text(), "a");
}

#[test]
fn literal_unicode_find_wraps_and_does_not_overlap() {
    let mut editor = editor("🦀 aa aaAA 🦀");
    assert_eq!(search(editor.state(), "").total, 0);
    assert_eq!(search(editor.state(), "aa").total, 2);
    let found = next_match(editor.state(), "🦀", false).unwrap();
    select(&mut editor, found.from.0, found.to.0);
    let second = next_match(editor.state(), "🦀", false).unwrap();
    assert!(second.from > found.from);
    select(&mut editor, second.from.0, second.to.0);
    assert_eq!(next_match(editor.state(), "🦀", false), Some(found));
    assert_eq!(next_match(editor.state(), "🦀", true), Some(found));
    assert_eq!(search(self::editor("aaa").state(), "aa").total, 1);
}

#[test]
fn cache_cap_does_not_cap_navigation_or_replacement_and_replace_all_undoes_once() {
    let mut editor = editor(&"a ".repeat(10_001));
    let result = search(editor.state(), "a");
    assert_eq!(result.total, 10_001);
    assert_eq!(result.matches.len(), 10_000);
    select(&mut editor, 19_998, 19_999);
    assert_eq!(
        next_match(editor.state(), "a", false),
        Some(TextRange::new(20_000, 20_001))
    );
    let transaction = replace_all(editor.state(), "a", "🦀", editor.limits()).unwrap();
    editor.dispatch(transaction, 0).unwrap();
    assert_eq!(editor.state().document().text(), "🦀 ".repeat(10_001));
    editor.undo().unwrap();
    assert_eq!(editor.state().document().text(), "a ".repeat(10_001));
    assert!(!editor.can_undo());
}

#[test]
fn replacement_revalidates_selection_and_growth_before_any_change() {
    let mut editor = editor("one two");
    select(&mut editor, 0, 3);
    assert!(
        replace_current(editor.state(), "two", "x")
            .unwrap()
            .is_none()
    );
    let limits = DocumentLimits {
        max_bytes: 8,
        max_lines: 1,
    };
    assert_eq!(
        replace_all(editor.state(), "one", "too long", limits).unwrap_err(),
        CodeError::SizeLimit
    );
    assert_eq!(editor.state().document().text(), "one two");
    assert!(!editor.can_undo());
    assert!(
        ChangeSet::default()
            .map_offset(editor.state().document(), TextOffset(99), Bias::After)
            .is_err()
    );
}
