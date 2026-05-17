//! Unit tests for the input-rules data types, the pure matching
//! engine, and the plugin state-field shape.
//!
//! C1 scope: data types + matcher + plugin shape. View-side
//! integration tests live in Phase 5 C2.

use regex::Regex;

use crate::model::{Attrs, Fragment};
use crate::schema_basic;
use crate::state::{EditorState, EditorStateConfig};

use super::plugin::{input_rules, rule_fire_meta, undo_input_rule, INPUT_RULES_PLUGIN_KEY};
use super::rule::{InCodePolicy, InputRule};
use super::run::run_rules;

fn state_with_paragraph(text: &str) -> EditorState {
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text(text.to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    EditorState::create(EditorStateConfig::new(schema, doc)).unwrap()
}

#[test]
fn replacement_handler_substitutes_full_match() {
    // `--` → `—`. No capture group, so the entire match is replaced.
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let state = state_with_paragraph("hello-");

    // Cursor at end of "hello-" — paragraph's open token sits at
    // doc pos 0, "hello-" fills pos 1..7, so position 7 is right
    // after the existing `-`. Typing another `-` makes the matcher
    // see "hello--" and fire the `--$` rule.
    let from = 7;
    let to = 7;
    let fire = run_rules(&state, from, to, "-", std::slice::from_ref(&rule))
        .expect("rule should match `--$` at end of buffer");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(
        para_text.contains('—'),
        "em-dash should land in the doc, got: {para_text:?}"
    );
    assert!(
        !para_text.contains("--"),
        "the original `--` should be gone"
    );
}

#[test]
fn replacement_handler_with_capture_group_substitutes_only_group_one() {
    // Smart-quote-style rule: only the captured `"` is replaced.
    let rule = InputRule::replacement(Regex::new(r#"(?:^|[\s])(")$"#).unwrap(), "“");
    let state = state_with_paragraph("Hello ");

    let from = 7; // pos at end of "Hello " (one past the trailing space)
    let to = 7;
    let fire = run_rules(&state, from, to, "\"", std::slice::from_ref(&rule))
        .expect("rule should match `\"` after whitespace");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(
        para_text.contains('“'),
        "curly opening quote should land, got: {para_text:?}"
    );
    assert!(
        !para_text.contains('"'),
        "the straight quote should be gone"
    );
}

#[test]
fn replacement_with_capture_group_preserves_prefix_in_document() {
    // Codex C1 regression: my first port of `string_handler`
    // prepended the prefix into the replacement string AND advanced
    // `replace_start` past the prefix — so the prefix ended up
    // duplicated. After the fix, the prefix STAYS in the document
    // and only the captured slice is replaced.
    let rule = InputRule::replacement(Regex::new(r#"(?:^|[\s])(")$"#).unwrap(), "“");
    let state = state_with_paragraph("Hello ");

    let fire = run_rules(&state, 7, 7, "\"", std::slice::from_ref(&rule)).expect("rule fires");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    // Exactly one space between "Hello" and the curly quote — no
    // duplicated prefix.
    assert_eq!(
        para_text, "Hello “",
        "prefix should appear exactly once, got: {para_text:?}"
    );
}

#[test]
fn rule_does_not_fire_when_pattern_doesnt_overlap_just_typed_text() {
    // Pattern matches but exclusively from prior buffer text — the
    // matcher should reject it because the match doesn't include
    // the just-typed character.
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let state = state_with_paragraph("foo-- bar");

    // Cursor at end of "foo-- bar" — pos 10 (1 for paragraph open
    // + 9 chars). Typing a space — no `--` pattern overlaps the
    // just-typed char.
    let from = 10;
    let to = 10;
    assert!(
        run_rules(&state, from, to, " ", std::slice::from_ref(&rule)).is_none(),
        "rule should not fire when match doesn't include the just-typed text"
    );
}

#[test]
fn closure_handler_can_emit_a_custom_transaction() {
    // A custom handler that uppercases the matched text.
    let rule = InputRule::new(
        Regex::new(r"hello$").unwrap(),
        |state, _captures, start, end| {
            let mut tr = state.tr();
            tr.delete(start, end).ok()?;
            tr.insert_text("HELLO").ok()?;
            Some(tr)
        },
    );
    let state = state_with_paragraph("hell");

    // "hell" at pos 1..5, cursor at end = pos 5.
    let from = 5;
    let to = 5;
    let fire = run_rules(&state, from, to, "o", std::slice::from_ref(&rule)).expect("rule fires");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(
        para_text.contains("HELLO"),
        "expected `HELLO`, got: {para_text:?}"
    );
}

#[test]
fn first_matching_rule_wins() {
    let first = InputRule::new(Regex::new(r"!$").unwrap(), |state, _c, start, end| {
        let mut tr = state.tr();
        tr.delete(start, end).ok()?;
        tr.insert_text("FIRST").ok()?;
        Some(tr)
    });
    let second = InputRule::new(Regex::new(r"!$").unwrap(), |state, _c, start, end| {
        let mut tr = state.tr();
        tr.delete(start, end).ok()?;
        tr.insert_text("SECOND").ok()?;
        Some(tr)
    });
    let rules = vec![first, second];
    let state = state_with_paragraph("hi");

    // "hi" → pos 3 at end.
    let fire = run_rules(&state, 3, 3, "!", &rules).expect("a rule fires");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(para_text.contains("FIRST"));
    assert!(!para_text.contains("SECOND"));
}

#[test]
fn no_rules_returns_none() {
    let state = state_with_paragraph("abc");
    assert!(run_rules(&state, 4, 4, " ", &[]).is_none());
}

#[test]
fn rule_returning_none_from_handler_falls_through_to_next_rule() {
    let first = InputRule::new(Regex::new(r"!$").unwrap(), |_state, _c, _start, _end| None);
    let second = InputRule::new(Regex::new(r"!$").unwrap(), |state, _c, start, end| {
        let mut tr = state.tr();
        tr.delete(start, end).ok()?;
        tr.insert_text("LANDED").ok()?;
        Some(tr)
    });
    let rules = vec![first, second];
    let state = state_with_paragraph("hi");

    let fire = run_rules(&state, 3, 3, "!", &rules).expect("second rule fires");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(para_text.contains("LANDED"));
}

#[test]
fn options_disallow_in_code_default() {
    // Default `in_code: Disallow` and `in_code_mark: Disallow`. We
    // can't easily fabricate a `code_block` cursor in pine-richtext
    // tests without more plumbing, but we can confirm the
    // `RuleOptions::default()` values are conservative.
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    assert!(matches!(rule.options().in_code, InCodePolicy::Disallow));
    assert!(matches!(
        rule.options().in_code_mark,
        InCodePolicy::Disallow
    ));
    assert!(rule.options().undoable);
}

#[test]
fn input_rules_plugin_has_key() {
    let plugin = input_rules();
    assert_eq!(plugin.key(), INPUT_RULES_PLUGIN_KEY);
}

#[test]
fn rule_fire_meta_round_trips() {
    let meta = rule_fire_meta(3, 5, "# ");
    assert_eq!(meta.get("from").and_then(|v| v.as_u64()), Some(3));
    assert_eq!(meta.get("to").and_then(|v| v.as_u64()), Some(5));
    assert_eq!(meta.get("text").and_then(|v| v.as_str()), Some("# "));
}

#[test]
fn plugin_state_clears_after_a_subsequent_doc_edit() {
    // Build a state that's wired with the input-rules plugin.
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("abc".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let plugin = input_rules();
    let state =
        EditorState::create(EditorStateConfig::new(schema, doc).plugins(vec![plugin])).unwrap();

    // Simulate a rule firing by attaching meta to the transaction.
    let mut tr = state.tr();
    tr.set_meta(INPUT_RULES_PLUGIN_KEY, rule_fire_meta(1, 4, "abc"));
    let after_rule = state.apply(tr).unwrap();
    assert_eq!(
        after_rule
            .plugin_state(INPUT_RULES_PLUGIN_KEY)
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str()),
        Some("abc"),
        "plugin state should record the rule-fire meta"
    );

    // A subsequent doc-edit transaction (no meta) should clear the
    // plugin's memory.
    let mut tr2 = after_rule.tr();
    tr2.insert_text("X").ok();
    let after_edit = after_rule.apply(tr2).unwrap();
    assert!(
        after_edit
            .plugin_state(INPUT_RULES_PLUGIN_KEY)
            .map(|v| v.is_null())
            .unwrap_or(true),
        "plugin state should clear after a non-rule edit"
    );
}

#[test]
fn plugin_state_survives_meta_only_transaction() {
    // Codex C1 regression: my first port of the apply() callback
    // used `tr.selection().is_some()` as the selection-changed
    // signal. But `state.tr()` pre-populates the transaction's
    // selection, so EVERY transaction had it set — any no-op /
    // meta-only transaction would wipe the rule memory.
    // After the fix, the callback compares old vs new selection
    // and only clears state on a genuine selection change.
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("abc".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let plugin = input_rules();
    let state =
        EditorState::create(EditorStateConfig::new(schema, doc).plugins(vec![plugin])).unwrap();

    // Fire a rule via meta.
    let mut seed = state.tr();
    seed.set_meta(INPUT_RULES_PLUGIN_KEY, rule_fire_meta(1, 4, "abc"));
    let after_rule = state.apply(seed).unwrap();
    assert!(
        after_rule
            .plugin_state(INPUT_RULES_PLUGIN_KEY)
            .map(|v| !v.is_null())
            .unwrap_or(false),
        "plugin state set after rule fire"
    );

    // A meta-only transaction (no steps, no selection change) must
    // NOT clear the plugin state.
    let mut noop = after_rule.tr();
    noop.set_meta("unrelated_key", serde_json::json!(true));
    let after_noop = after_rule.apply(noop).unwrap();
    assert!(
        after_noop
            .plugin_state(INPUT_RULES_PLUGIN_KEY)
            .map(|v| !v.is_null())
            .unwrap_or(false),
        "plugin state should survive meta-only transactions"
    );
}

#[test]
fn run_rules_reports_post_rule_output_range_and_original_text() {
    // Codex C1 regression #5: `RuleFire` must carry the rule's
    // OUTPUT range (in the post-rule doc) and the ORIGINAL matched
    // text — not the caller's input `from`/`to`/`text`. Undo
    // replaces `[from, to]` with `text` to roll the rule back.
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let state = state_with_paragraph("hello-");
    let fire = run_rules(&state, 7, 7, "-", std::slice::from_ref(&rule)).expect("rule fires");
    // `--` rule deleted `[6, 7]` and inserted `—` at 6. Post-rule
    // output spans `[6, 7]` (one em-dash char). Original undo
    // text = pre-rule slice "-" + typed "-" = "--".
    assert_eq!(fire.from, 6, "output starts at matched-range start");
    assert_eq!(fire.to, 7, "output ends at post-rule cursor");
    assert_eq!(
        fire.text, "--",
        "undo text = pre-rule replaced slice + typed char"
    );
    assert!(fire.undoable, "default rule options are undoable");
}

#[test]
fn capture_group_rule_undo_does_not_over_restore_prefix() {
    // Codex C1 regression #7: for capture-group rules with leading
    // context (e.g. smart-quote rule `(?:^|[\s])(")$`), the rule
    // only replaces the captured `"` — the preceding space stays
    // in the doc. Undo should restore JUST the typed `"`, not the
    // full match ` "` (which would re-insert the space).
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("Hello ".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let plugin = input_rules();
    let state =
        EditorState::create(EditorStateConfig::new(schema, doc).plugins(vec![plugin])).unwrap();

    let rule = InputRule::replacement(Regex::new(r#"(?:^|[\s])(")$"#).unwrap(), "“");
    let fire = run_rules(&state, 7, 7, "\"", std::slice::from_ref(&rule)).expect("rule fires");
    assert_eq!(
        fire.text, "\"",
        "undo text should be just the typed quote, not the full match ` \"`"
    );

    // End-to-end: fire + undo should land on `Hello "` (with a
    // straight quote), NOT `Hello  "` (with a duplicated space).
    let mut tr = fire.transaction;
    tr.set_meta(
        INPUT_RULES_PLUGIN_KEY,
        rule_fire_meta(fire.from, fire.to, &fire.text),
    );
    let after_rule = state.apply(tr).unwrap();
    let after_rule_text: String = after_rule.doc().child(0).unwrap().text_content();
    assert_eq!(after_rule_text, "Hello “");

    let undo_cmd = undo_input_rule();
    let undo_tr = undo_cmd.apply(&after_rule).expect("undo applies");
    let after_undo = after_rule.apply(undo_tr).unwrap();
    let after_undo_text: String = after_undo.doc().child(0).unwrap().text_content();
    assert_eq!(
        after_undo_text, "Hello \"",
        "undo restores the typed quote without duplicating the prefix space"
    );
}

#[test]
fn rule_over_selection_does_not_resurrect_selection_on_undo() {
    // Codex C1 round-4 regression: when a rule fires over a non-
    // empty selection (`from < to`), the undo payload must NOT
    // include the selected text — normal typing would have
    // replaced the selection with the typed character, so undo
    // should restore the typed character at that position, not
    // bring the selection back.
    //
    // Scenario: doc is "hello-X" (7 chars). User selects `X` and
    // types `-`. Rule sees buffer "hello-" + "-" = "hello--" and
    // fires `--$` → `—`. Without the fix, undo would produce
    // "hello-X-" (the X resurrected); with the fix, undo gives
    // "hello--".
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("hello-X".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let plugin = input_rules();
    let state =
        EditorState::create(EditorStateConfig::new(schema, doc).plugins(vec![plugin])).unwrap();

    // Selection covers `X`: positions [7, 8].
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let fire = run_rules(&state, 7, 8, "-", std::slice::from_ref(&rule)).expect("rule fires");

    // The undo payload should be `--`, NOT `-X-`. Pre-consumed slice
    // is `[output_from=6, from=7]` = "-"; typed "-" appended.
    assert_eq!(
        fire.text, "--",
        "undo text should not include the selected `X`, got: {:?}",
        fire.text
    );

    // End-to-end: fire + undo should restore "hello--", not "hello-X-".
    let mut tr = fire.transaction;
    tr.set_meta(
        INPUT_RULES_PLUGIN_KEY,
        rule_fire_meta(fire.from, fire.to, &fire.text),
    );
    let after_rule = state.apply(tr).unwrap();
    let after_rule_text: String = after_rule.doc().child(0).unwrap().text_content();
    assert_eq!(after_rule_text, "hello—");

    let undo_cmd = undo_input_rule();
    let undo_tr = undo_cmd.apply(&after_rule).expect("undo applies");
    let after_undo = after_rule.apply(undo_tr).unwrap();
    let after_undo_text: String = after_undo.doc().child(0).unwrap().text_content();
    assert_eq!(
        after_undo_text, "hello--",
        "undo should restore typed `-` over the selection, not resurrect `X`"
    );
}

#[test]
fn replacement_inherits_marks_at_mark_boundary() {
    // Codex C1 regression #9: when the matched range starts at a
    // mark boundary (unmarked text on the left, mark span on the
    // right), the cursor is inside the right side's marks but
    // `replace_start` resolves to the BOUNDARY where `marks()`
    // returns the left side's marks. Resolving at the cursor `end`
    // instead always sees the active mark span the user is typing
    // inside.
    let schema = schema_basic::schema();
    let strong = schema.mark("strong", Attrs::new()).unwrap();
    let plain_prefix = schema.text("a ".to_string(), Vec::new()).unwrap();
    let bold_dash = schema.text("-".to_string(), vec![strong.clone()]).unwrap();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(vec![plain_prefix, bold_dash]),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let state = EditorState::create(EditorStateConfig::new(schema, doc)).unwrap();

    // Cursor at end (pos 4, inside the strong span). Typing `-`
    // should produce a STRONG em-dash, not an unmarked one.
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let fire = run_rules(&state, 4, 4, "-", std::slice::from_ref(&rule)).expect("rule fires");
    let next = state.apply(fire.transaction).unwrap();

    let para = next.doc().child(0).unwrap();
    let mut found_strong_em_dash = false;
    for i in 0..para.child_count() {
        let child = para.child(i).unwrap();
        if let Some(text) = child.text() {
            if text.contains('—') && child.marks().iter().any(|m| m.type_name() == "strong") {
                found_strong_em_dash = true;
            }
        }
    }
    assert!(
        found_strong_em_dash,
        "em-dash at the mark boundary should inherit the strong mark from the cursor's side"
    );
}

#[test]
fn run_rules_carries_undoable_flag_from_rule_options() {
    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—").not_undoable();
    let state = state_with_paragraph("hello-");
    let fire = run_rules(&state, 7, 7, "-", std::slice::from_ref(&rule)).expect("rule fires");
    assert!(!fire.undoable, "not_undoable() flag should surface");
}

#[test]
fn end_to_end_undo_restores_original_after_rule_fire() {
    // Codex C1 regression #5: the full fire → meta → undo loop
    // must restore the typed character. Em-dash rule: typing
    // `--` produces `—`; Backspace undo should give `--` back.
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("hello-".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let plugin = input_rules();
    let state =
        EditorState::create(EditorStateConfig::new(schema, doc).plugins(vec![plugin])).unwrap();

    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let fire = run_rules(&state, 7, 7, "-", std::slice::from_ref(&rule)).expect("fires");

    // Attach the rule-fire meta and dispatch.
    let mut tr = fire.transaction;
    tr.set_meta(
        INPUT_RULES_PLUGIN_KEY,
        rule_fire_meta(fire.from, fire.to, &fire.text),
    );
    let after_rule = state.apply(tr).unwrap();
    let text_after_rule: String = after_rule.doc().child(0).unwrap().text_content();
    assert!(text_after_rule.contains('—'));
    assert!(!text_after_rule.contains("--"));

    // Backspace undo.
    let undo_cmd = undo_input_rule();
    let undo_tr = undo_cmd.apply(&after_rule).expect("undo applies");
    let after_undo = after_rule.apply(undo_tr).unwrap();
    let text_after_undo: String = after_undo.doc().child(0).unwrap().text_content();
    assert!(
        text_after_undo.contains("--"),
        "undo should restore the literal `--`, got: {text_after_undo:?}"
    );
    assert!(!text_after_undo.contains('—'));
}

#[test]
fn replacement_inherits_marks_from_cursor_position() {
    // Codex C1 regression #6: my first port of the explicit-
    // position `replace_with` path created the replacement text
    // node with `Vec::new()` marks, stripping any active formatting.
    // The fix derives marks from the cursor's resolved position.
    let schema = schema_basic::schema();
    let strong = schema.mark("strong", Attrs::new()).unwrap();
    let bold_text = schema
        .text("hello-".to_string(), vec![strong.clone()])
        .unwrap();
    let para = schema
        .node("paragraph", Attrs::new(), Fragment::from(bold_text))
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let state = EditorState::create(EditorStateConfig::new(schema, doc)).unwrap();

    let rule = InputRule::replacement(Regex::new(r"--$").unwrap(), "—");
    let fire = run_rules(&state, 7, 7, "-", std::slice::from_ref(&rule)).expect("rule fires");
    let next = state.apply(fire.transaction).unwrap();

    // The em-dash should still be wrapped in `<strong>` — i.e. the
    // text node at the rule's output range carries the strong mark.
    let para = next.doc().child(0).unwrap();
    let mut found_em_dash_with_strong = false;
    for i in 0..para.child_count() {
        let child = para.child(i).unwrap();
        if let Some(text) = child.text() {
            if text.contains('—') && child.marks().iter().any(|m| m.type_name() == "strong") {
                found_em_dash_with_strong = true;
            }
        }
    }
    assert!(
        found_em_dash_with_strong,
        "em-dash should inherit strong mark from the surrounding text"
    );
}

#[test]
fn undo_input_rule_restores_original_text() {
    // Build a state with the input-rules plugin + a paragraph whose
    // text already reflects a rule's output (`HELLO`), and the
    // plugin state remembers the original text was `hello`.
    let schema = schema_basic::schema();
    let para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("HELLO".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();
    let plugin = input_rules();
    let state =
        EditorState::create(EditorStateConfig::new(schema, doc).plugins(vec![plugin])).unwrap();
    // Seed the plugin state: rule fired at [1, 6], original text "hello".
    let mut seed_tr = state.tr();
    seed_tr.set_meta(INPUT_RULES_PLUGIN_KEY, rule_fire_meta(1, 6, "hello"));
    let after_rule = state.apply(seed_tr).unwrap();

    let cmd = undo_input_rule();
    let tr = cmd.apply(&after_rule).expect("undo applies");
    let next = after_rule.apply(tr).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(
        para_text.contains("hello"),
        "expected original `hello` after undo, got: {para_text:?}"
    );
    assert!(
        !para_text.contains("HELLO"),
        "rule output should be gone after undo"
    );
}

#[test]
fn undo_input_rule_returns_none_when_no_rule_has_fired() {
    let state = state_with_paragraph("abc");
    let cmd = undo_input_rule();
    assert!(cmd.apply(&state).is_none());
}

#[test]
fn runtime_collects_input_rules_from_extension() {
    // C2: `RichTextExtension::input_rules()` flows into
    // `EditorRuntime::input_rules()` so the view-side hook can
    // read the merged rule list from the runtime.
    use crate::extension::RichTextExtension;
    use crate::runtime::RuntimeBuilder;

    struct ExtA;
    impl RichTextExtension for ExtA {
        fn name(&self) -> &str {
            "ext-a"
        }
        fn input_rules(&self) -> Vec<InputRule> {
            vec![InputRule::replacement(Regex::new(r"AAA$").unwrap(), "A!")]
        }
    }
    struct ExtB;
    impl RichTextExtension for ExtB {
        fn name(&self) -> &str {
            "ext-b"
        }
        fn input_rules(&self) -> Vec<InputRule> {
            vec![InputRule::replacement(Regex::new(r"BBB$").unwrap(), "B!")]
        }
    }

    let runtime = RuntimeBuilder::new()
        .without_defaults()
        .with(crate::extensions::CoreNodesExtension) // schema needs `doc`
        .with(ExtA)
        .with(ExtB)
        .build();
    let merged = runtime.input_rules();
    assert_eq!(merged.len(), 2, "both extensions should contribute rules");
    let patterns: Vec<&str> = merged.iter().map(|r| r.regex().as_str()).collect();
    assert!(patterns.contains(&"AAA$"));
    assert!(patterns.contains(&"BBB$"));
}

#[test]
fn runtime_input_rules_ordering_is_user_first_then_base() {
    // Codex C2 contract: input rules are first-MATCH-wins; the
    // builder folds USER extensions FIRST (in registration order),
    // then BASE extensions whose name isn't shadowed. Apps that
    // `.with(MyExt)` get their rules ahead of built-in rules.
    //
    // This test uses the test-only `build_with_explicit_base_for_tests`
    // hook so `BaseStubExt` is injected into the real base chain —
    // verifying the user-vs-base path, not just user registration
    // order.
    use crate::extension::RichTextExtension;
    use crate::runtime::RuntimeBuilder;
    use std::sync::Arc;

    struct BaseStubExt;
    impl RichTextExtension for BaseStubExt {
        fn name(&self) -> &str {
            "base-stub"
        }
        fn input_rules(&self) -> Vec<InputRule> {
            vec![InputRule::replacement(
                Regex::new(r"\$base$").unwrap(),
                "BASE",
            )]
        }
    }
    struct UserExt;
    impl RichTextExtension for UserExt {
        fn name(&self) -> &str {
            "user-ext"
        }
        fn input_rules(&self) -> Vec<InputRule> {
            vec![InputRule::replacement(
                Regex::new(r"\$user$").unwrap(),
                "USER",
            )]
        }
    }

    let base: Vec<Arc<dyn RichTextExtension>> = vec![
        Arc::new(crate::extensions::CoreNodesExtension),
        Arc::new(BaseStubExt),
    ];
    let runtime = RuntimeBuilder::new()
        .with(UserExt)
        .build_with_explicit_base_for_tests(base);

    let patterns: Vec<&str> = runtime
        .input_rules()
        .iter()
        .map(|r| r.regex().as_str())
        .collect();
    // UserExt is in user chain; BaseStubExt is in base chain. The
    // builder folds user-first, so UserExt's pattern appears
    // before BaseStubExt's. A future refactor that flips this
    // (folds base first into the rule list) would break this
    // assertion.
    assert_eq!(patterns, vec!["\\$user$", "\\$base$"]);
}

#[test]
fn smart_typography_extension_em_dash_fires() {
    // C3 acceptance: SmartTypographyExtension's em-dash rule fires
    // end-to-end through the runtime's input-rules list.
    use crate::extensions::SmartTypographyExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new().with(SmartTypographyExtension).build();
    assert!(
        !runtime.input_rules().is_empty(),
        "smart-typography contributes rules"
    );

    let state = state_with_paragraph("hello-");
    let fire = run_rules(&state, 7, 7, "-", runtime.input_rules()).expect("em-dash fires");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert!(
        para_text.contains('—'),
        "em-dash should land in the doc, got: {para_text:?}"
    );
}

#[test]
fn smart_typography_extension_opens_double_quote_after_space() {
    use crate::extensions::SmartTypographyExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new().with(SmartTypographyExtension).build();
    let state = state_with_paragraph("Hello ");
    let fire = run_rules(&state, 7, 7, "\"", runtime.input_rules()).expect("open-quote fires");
    let next = state.apply(fire.transaction).unwrap();
    let para_text: String = next.doc().child(0).unwrap().text_content();
    assert_eq!(para_text, "Hello \u{201C}");
}

#[test]
fn markdown_shortcuts_extension_heading_fires() {
    // C3 acceptance: typing `# ` at start of a paragraph converts
    // it to a heading (level 1).
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let state = state_with_paragraph("#");
    let fire = run_rules(&state, 2, 2, " ", runtime.input_rules()).expect("heading fires");
    let next = state.apply(fire.transaction).unwrap();
    let block = next.doc().child(0).unwrap();
    assert_eq!(block.type_name(), "heading");
    assert_eq!(
        block.attrs().get("level").and_then(|v| v.as_u64()),
        Some(1),
        "single `#` produces an H1"
    );
}

#[test]
fn markdown_shortcuts_extension_h3_fires() {
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let state = state_with_paragraph("###");
    let fire = run_rules(&state, 4, 4, " ", runtime.input_rules()).expect("heading fires");
    let next = state.apply(fire.transaction).unwrap();
    let block = next.doc().child(0).unwrap();
    assert_eq!(block.type_name(), "heading");
    assert_eq!(
        block.attrs().get("level").and_then(|v| v.as_u64()),
        Some(3),
        "triple `#` produces an H3"
    );
}

#[test]
fn markdown_shortcuts_extension_bullet_list_fires() {
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let state = state_with_paragraph("*");
    let fire = run_rules(&state, 2, 2, " ", runtime.input_rules()).expect("bullet fires");
    let next = state.apply(fire.transaction).unwrap();
    let top = next.doc().child(0).unwrap();
    assert_eq!(
        top.type_name(),
        "bullet_list",
        "asterisk shortcut should wrap in bullet_list"
    );
}

#[test]
fn markdown_heading_shortcut_places_cursor_inside_new_block() {
    // Codex C3 regression #1: after the heading shortcut fires, the
    // cursor must land INSIDE the new heading so the next
    // `insertText` event types into it. Without the explicit
    // `set_selection(text(start))` the selection mapping lands
    // OUTSIDE the converted block and the next character appears
    // in a sibling paragraph.
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let state = state_with_paragraph("#");
    let fire = run_rules(&state, 2, 2, " ", runtime.input_rules()).expect("heading fires");
    let next = state.apply(fire.transaction).unwrap();

    // The new heading is at position 0..2 (open token + close
    // token, content empty). The cursor should land at content
    // start = position 1 (inside the heading), NOT position 2 or
    // beyond.
    let sel = next.selection();
    let head = match sel {
        crate::state::Selection::Text { head, .. } => *head,
        _ => panic!("expected text selection, got: {:?}", sel),
    };
    let resolved = next.doc().resolve(head).unwrap();
    assert_eq!(
        resolved.parent().type_name(),
        "heading",
        "cursor's parent should be the heading after shortcut fires; resolved parent: {:?}",
        resolved.parent().type_name(),
    );
}

#[test]
fn markdown_bullet_shortcut_joins_with_preceding_list() {
    // Codex C3 regression #2: when a bullet shortcut fires after
    // an existing bullet_list, the new wrapper should auto-join
    // with the prior one (single list, two items). Without the
    // join-position fix the resolver landed inside the prior
    // wrapper's content and `node_before()` returned the wrong
    // node, so the rule produced two sibling bullet_lists.
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    // Build a doc with an existing bullet list followed by a
    // paragraph containing the marker `*`.
    let schema = schema_basic::schema();
    let item_text = schema.text("foo".to_string(), Vec::new()).unwrap();
    let item_para = schema
        .node("paragraph", Attrs::new(), Fragment::from(item_text))
        .unwrap();
    let list_item = schema
        .node("list_item", Attrs::new(), Fragment::from(item_para))
        .unwrap();
    let bullet = schema
        .node("bullet_list", Attrs::new(), Fragment::from(list_item))
        .unwrap();
    let marker_para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("*".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![bullet, marker_para]),
        )
        .unwrap();
    let state = EditorState::create(EditorStateConfig::new(schema, doc)).unwrap();

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    // Cursor at end of marker paragraph's `*` content (one char of
    // content + paragraph close at the very end). Doc shape:
    // `<bullet_list>foo</bullet_list><p>*[CURSOR]</p>` — the cursor
    // sits INSIDE the marker paragraph, right before its close
    // token. `content_size - 1` is that position.
    let cursor = state.doc().content_size() - 1;
    let fire = run_rules(&state, cursor, cursor, " ", runtime.input_rules())
        .expect("bullet shortcut rule should fire on `* `");
    let next = state.apply(fire.transaction).unwrap();

    // After the rule + auto-join, the doc has ONE bullet_list with
    // TWO items (the original + the newly wrapped empty paragraph).
    assert_eq!(
        next.doc().child_count(),
        1,
        "should have one top-level node after auto-join, got: {:?}",
        next.doc().child_count()
    );
    let top = next.doc().child(0).unwrap();
    assert_eq!(top.type_name(), "bullet_list");
    assert_eq!(top.child_count(), 2, "list should have 2 items after join");
}

#[test]
fn markdown_bullet_shortcut_places_cursor_inside_joined_list_item() {
    // Codex C3 regression #4: when the bullet shortcut auto-joins
    // with a preceding bullet_list, the cursor must end up INSIDE
    // the new (empty) list item so the next keystroke types into
    // it. Without setting the selection before the join, the
    // mapped selection ends up at the end of the doc and typing
    // appears outside the list entirely.
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    // Same doc setup as the join test: existing bullet_list +
    // marker paragraph with `*`.
    let schema = schema_basic::schema();
    let item_para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("foo".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let list_item = schema
        .node("list_item", Attrs::new(), Fragment::from(item_para))
        .unwrap();
    let bullet = schema
        .node("bullet_list", Attrs::new(), Fragment::from(list_item))
        .unwrap();
    let marker_para = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("*".to_string(), Vec::new()).unwrap()),
        )
        .unwrap();
    let doc = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![bullet, marker_para]),
        )
        .unwrap();
    let state = EditorState::create(EditorStateConfig::new(schema, doc)).unwrap();

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let cursor = state.doc().content_size() - 1;
    let fire = run_rules(&state, cursor, cursor, " ", runtime.input_rules())
        .expect("bullet shortcut fires");
    let next = state.apply(fire.transaction).unwrap();
    let sel = next.selection();
    let head = match sel {
        crate::state::Selection::Text { head, .. } => *head,
        _ => panic!("expected text selection, got: {:?}", sel),
    };
    let resolved = next.doc().resolve(head).unwrap();
    let mut found_list_item = false;
    for depth in 0..=resolved.depth() {
        if let Some(node) = resolved.node(depth) {
            if node.type_name() == "list_item" {
                found_list_item = true;
                break;
            }
        }
    }
    assert!(
        found_list_item,
        "cursor should be inside a list_item ancestor after auto-join, but \
         the resolved path was: {:?}",
        (0..=resolved.depth())
            .map(|d| resolved.node(d).map(|n| n.type_name().to_string()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn markdown_shortcuts_are_not_undoable_so_backspace_deletes_normally() {
    // Codex C3 regression #3: block-shortcut rules (heading,
    // bullet, blockquote, code_block, ordered list) emit
    // multiple steps. `run_rules` records undo metadata from
    // only the first step map, so Backspace's undo would
    // produce a garbled doc. C3 marks the builder rules
    // `not_undoable()` until a multi-step undo strategy lands.
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    for rule in runtime.input_rules() {
        assert!(
            !rule.options().undoable,
            "markdown-shortcut rule should be marked not_undoable, but regex `{}` had undoable=true",
            rule.regex().as_str()
        );
    }
}

#[test]
fn markdown_shortcuts_extension_blockquote_fires() {
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let state = state_with_paragraph(">");
    let fire = run_rules(&state, 2, 2, " ", runtime.input_rules()).expect("blockquote fires");
    let next = state.apply(fire.transaction).unwrap();
    let top = next.doc().child(0).unwrap();
    assert_eq!(
        top.type_name(),
        "blockquote",
        "`> ` shortcut should wrap in blockquote"
    );
}

#[test]
fn markdown_shortcuts_extension_code_block_fires() {
    use crate::extensions::MarkdownShortcutsExtension;
    use crate::runtime::RuntimeBuilder;

    let runtime = RuntimeBuilder::new()
        .with(MarkdownShortcutsExtension)
        .build();
    let state = state_with_paragraph("``");
    let fire = run_rules(&state, 3, 3, "`", runtime.input_rules()).expect("code_block fires");
    let next = state.apply(fire.transaction).unwrap();
    let block = next.doc().child(0).unwrap();
    assert_eq!(
        block.type_name(),
        "code_block",
        "triple-backtick should set block type to code_block"
    );
}

#[test]
fn runtime_default_includes_input_rules_plugin() {
    // C2: every runtime (even minimal builds) automatically
    // includes the input-rules state-tracking plugin so
    // `undo_input_rule` can read its memory regardless of which
    // extensions an app registers.
    use crate::runtime::RuntimeBuilder;
    let runtime = RuntimeBuilder::new()
        .without_defaults()
        .with(crate::extensions::CoreNodesExtension)
        .build();
    assert!(
        runtime
            .plugins()
            .iter()
            .any(|p| p.key() == INPUT_RULES_PLUGIN_KEY),
        "input-rules plugin should be installed in every runtime"
    );
}
