//! Parity tests for `prosemirror-state/test/*.ts`.
//!
//! Split out of the previous monolithic `prosemirror_parity.rs`. Mirrors
//! `prosemirror-state/test/test-state.ts` and `test-selection.ts`.

use serde_json::json;

mod support;

use pine_richtext::model::{Attrs, Fragment, Mark, NodeSpec, Schema};
use pine_richtext::schema_basic;
use pine_richtext::state::{EditorState, Plugin, Selection, META_APPENDED_TRANSACTION};

use support::*;

#[test]
fn state_selection_rejects_non_selectable_node_type() {
    // Build a schema where `widget` is a non-selectable leaf block.
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("block+"))
        .node(NodeSpec::new("paragraph").group("block").content("text*"))
        .node(
            NodeSpec::new("widget")
                .group("block")
                .atom()
                .non_selectable(),
        )
        .node(NodeSpec::new("text").inline())
        .finish()
        .unwrap();

    let widget = schema
        .node("widget", Attrs::new(), Fragment::empty())
        .unwrap();
    let paragraph = schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text("ok", Vec::new()).unwrap()),
        )
        .unwrap();
    let document = schema
        .node("doc", Attrs::new(), Fragment::from(vec![widget, paragraph]))
        .unwrap();
    let state = EditorState::create(pine_richtext::state::EditorStateConfig::new(
        schema, document,
    ))
    .unwrap();

    let mut tr = state.tr();
    let err = tr.set_selection(Selection::node(0)).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("not selectable"),
        "expected selectable error, got: {err}"
    );

    // Selecting the paragraph (selectable) still works.
    let widget_size = state.doc().content().child(0).unwrap().node_size();
    tr.set_selection(Selection::node(widget_size)).unwrap();
}

#[test]
fn state_json_preserves_selection_stored_marks_and_plugin_state() {
    let counter = Plugin::builder("count")
        .state_field(
            |_| Ok(json!(0)),
            |transaction, value, _, _| {
                Ok(json!(
                    value.as_u64().unwrap_or(0) + transaction.transform().steps().len() as u64
                ))
            },
        )
        .finish();

    let state = starter_state().reconfigure(vec![counter.clone()]).unwrap();
    let mut tr = state.tr();
    tr.set_selection(Selection::text(2)).unwrap();
    tr.delete(1, 2).unwrap();
    tr.set_stored_marks(vec![schema_basic::em().unwrap()]);
    let state = state.apply(tr).unwrap();

    let json = state.to_json().unwrap();
    let copy = EditorState::from_json(schema_basic::schema(), vec![counter], json).unwrap();

    assert_eq!(copy.doc(), state.doc());
    assert_eq!(copy.selection(), state.selection());
    assert_eq!(copy.stored_marks().unwrap()[0].type_name(), "em");
    assert_eq!(copy.plugin_state("count"), Some(&json!(1)));
}

#[test]
fn state_delete_selection_preserves_inclusive_marks() {
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo").into(),
        tagged_marked_text("<a>bar<b>", vec![em]).into(),
        tagged_text("baz").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);

    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    tr.delete_selection().unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(state.doc().text_content(), "foobaz");
    assert_eq!(state.stored_marks().unwrap()[0].type_name(), "em");
}

#[test]
fn state_delete_selection_drops_non_inclusive_boundary_marks() {
    let link = schema_basic::link("https://example.com", Option::<String>::None).unwrap();
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo").into(),
        tagged_marked_text("<a>bar<b>", vec![link, em]).into(),
        tagged_text("baz").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);

    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    tr.delete_selection().unwrap();
    let state = state.apply(tr).unwrap();

    let names = state
        .stored_marks()
        .unwrap()
        .iter()
        .map(Mark::type_name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["em"]);
}

#[test]
fn state_delete_selection_does_not_preserve_marks_at_block_end() {
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![
        tagged_paragraph(vec![
            tagged_text("foo").into(),
            tagged_marked_text("bar<a>", vec![em]).into(),
        ])
        .into(),
        tagged_paragraph_text("b<b>az").into(),
    ]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);

    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    tr.delete_selection().unwrap();
    let state = state.apply(tr).unwrap();

    assert!(state.stored_marks().is_none());
}

#[test]
fn state_delete_selection_keeps_non_inclusive_marks_when_still_inside_them() {
    let link = schema_basic::link("https://example.com", Option::<String>::None).unwrap();
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo").into(),
        tagged_marked_text("b", vec![link.clone()]).into(),
        tagged_marked_text("<a>a<b>", vec![link, em]).into(),
        tagged_marked_text(
            "r",
            vec![schema_basic::link("https://example.com", Option::<String>::None).unwrap()],
        )
        .into(),
        tagged_text("baz").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);

    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    tr.delete_selection().unwrap();
    let state = state.apply(tr).unwrap();

    let names = state
        .stored_marks()
        .unwrap()
        .iter()
        .map(Mark::type_name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["link", "em"]);
}

#[test]
fn state_insert_text_inherits_marks_when_replacing_marked_text() {
    let em = schema_basic::em().unwrap();
    let tagged = tagged_doc(vec![tagged_paragraph(vec![
        tagged_text("foo ").into(),
        tagged_marked_text("<a>bar<b>", vec![em.clone()]).into(),
        tagged_text(" baz").into(),
    ])
    .into()]);
    let from = tagged.tag("a");
    let to = tagged.tag("b");
    let state = state_with_doc(tagged.node);

    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(from, to)).unwrap();
    tr.insert_text("quux").unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(state.doc().text_content(), "foo quux baz");
    assert!(state.doc().range_has_mark(from, from + 4, &em).unwrap());
}

#[test]
fn state_replace_selection_places_cursor_after_inserted_inline_content() {
    let state = state_with_doc(doc(vec![paragraph(vec![
        text("foo"),
        image("one.png"),
        text("bar"),
        image("two.png"),
        text("baz"),
    ])]));

    let mut tr = state.tr();
    tr.set_selection(Selection::node(4)).unwrap();
    tr.replace_selection_with(hard_break()).unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(
        state.doc(),
        &doc(vec![paragraph(vec![
            text("foo"),
            hard_break(),
            text("bar"),
            image("two.png"),
            text("baz"),
        ])])
    );
    assert_eq!(state.selection(), &Selection::text(5));

    let mut tr = state.tr();
    tr.set_selection(Selection::node(8)).unwrap();
    tr.insert_text("abc").unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(
        state.doc(),
        &doc(vec![paragraph(vec![
            text("foo"),
            hard_break(),
            text("barabcbaz"),
        ])])
    );
    assert_eq!(state.selection(), &Selection::text(11));
}

#[test]
fn state_delete_selection_removes_selected_leaf_blocks() {
    let document = doc(vec![
        paragraph_text("a"),
        schema_basic::horizontal_rule().unwrap(),
        schema_basic::horizontal_rule().unwrap(),
        paragraph_text("b"),
    ]);
    let state = state_with_doc(document);

    let mut tr = state.tr();
    tr.set_selection(Selection::node(3)).unwrap();
    tr.delete_selection().unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(state.doc().child_count(), 3);
    assert_eq!(
        state.doc().content().child(1).unwrap().type_name(),
        "horizontal_rule"
    );

    let mut tr = state.tr();
    tr.delete_selection().unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(state.doc().child_count(), 2);
    assert_eq!(state.doc().text_content(), "ab");
}

#[test]
fn state_text_selection_between_adjusts_like_upstream() {
    let schema = schema_basic::schema();

    let tagged = tagged_doc(vec![tagged_paragraph_text("f<a>o<b>o").into()]);
    assert_eq!(
        Selection::text_between_near(
            &tagged.node,
            &schema,
            tagged.tag("b"),
            tagged.tag("a"),
            None
        )
        .unwrap(),
        Selection::text_between(tagged.tag("b"), tagged.tag("a"))
    );

    let tagged = tagged_doc(vec![tag("a"), tagged_paragraph_text("foo").into()]);
    assert_eq!(
        Selection::text_between_near(
            &tagged.node,
            &schema,
            tagged.tag("a"),
            tagged.tag("a"),
            None
        )
        .unwrap(),
        Selection::text(1)
    );

    let tagged = tagged_doc(vec![
        tagged_paragraph_text("foo").into(),
        tag("between"),
        tagged_paragraph_text("bar").into(),
    ]);
    assert_eq!(
        Selection::text_between_near(
            &tagged.node,
            &schema,
            tagged.tag("between"),
            tagged.tag("between"),
            Some(-1),
        )
        .unwrap(),
        Selection::text(4)
    );
    assert_eq!(
        Selection::text_between_near(
            &tagged.node,
            &schema,
            tagged.tag("between"),
            tagged.tag("between"),
            Some(1),
        )
        .unwrap(),
        Selection::text(6)
    );

    let tagged = tagged_doc(vec![
        tag("a"),
        tagged_paragraph_text("foo").into(),
        tag("b"),
    ]);
    assert_eq!(
        Selection::text_between_near(
            &tagged.node,
            &schema,
            tagged.tag("a"),
            tagged.tag("b"),
            None
        )
        .unwrap(),
        Selection::text_between(1, 4)
    );
    assert_eq!(
        Selection::text_between_near(
            &tagged.node,
            &schema,
            tagged.tag("b"),
            tagged.tag("a"),
            None
        )
        .unwrap(),
        Selection::text_between(4, 1)
    );
}

#[test]
fn state_selection_near_falls_back_to_node_or_all_selection() {
    let schema = Schema::builder()
        .node(NodeSpec::new("doc").content("horizontal_rule+"))
        .node(NodeSpec::new("horizontal_rule"))
        .finish()
        .unwrap();
    let rule = || {
        schema
            .node("horizontal_rule", Attrs::new(), Fragment::empty())
            .unwrap()
    };
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(rule()))
        .unwrap();

    assert_eq!(Selection::at_start(&doc, &schema), Selection::node(0));
    assert_eq!(Selection::at_end(&doc, &schema), Selection::node(0));
    assert_eq!(
        Selection::near(&doc, &schema, doc.content_size(), 1).unwrap(),
        Selection::node(0)
    );
    assert_eq!(
        Selection::text_between_near(&doc, &schema, doc.content_size(), doc.content_size(), None)
            .unwrap(),
        Selection::node(0)
    );

    let empty_schema = Schema::builder()
        .node(NodeSpec::new("doc"))
        .finish()
        .unwrap();
    let empty_doc = empty_schema
        .node("doc", Attrs::new(), Fragment::empty())
        .unwrap();
    assert_eq!(
        Selection::near(&empty_doc, &empty_schema, 0, 1).unwrap(),
        Selection::All
    );
}

#[test]
fn state_selection_follows_inserted_text_changes() {
    let state = state_with_doc(doc(vec![paragraph_text("hi")]));

    let mut tr = state.tr();
    tr.set_selection(Selection::text(1)).unwrap();
    tr.insert(1, Fragment::from(text("xy"))).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.selection(), &Selection::text(3));

    let mut tr = state.tr();
    tr.insert(1, Fragment::from(text("zq"))).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.selection(), &Selection::text(5));

    // An insertion past the selection does not move it.
    let mut tr = state.tr();
    tr.insert(7, Fragment::from(text("uv"))).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.selection(), &Selection::text(5));
}

#[test]
fn state_replace_selection_places_cursor_after_inserted_text() {
    let state = state_with_doc(doc(vec![paragraph_text("hi")]));

    let mut tr = state.tr();
    tr.set_selection(Selection::text_between(2, 3)).unwrap();
    tr.insert_text("o").unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(state.doc().text_content(), "ho");
    assert_eq!(state.selection(), &Selection::text(3));
}

#[test]
fn state_replace_selection_with_leaf_node_keeps_neighboring_text() {
    let state = state_with_doc(doc(vec![paragraph_text("foobar")]));

    let mut tr = state.tr();
    tr.set_selection(Selection::text(4)).unwrap();
    tr.replace_selection_with(horizontal_rule()).unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(
        state.doc(),
        &doc(vec![
            paragraph_text("foo"),
            horizontal_rule(),
            paragraph_text("bar"),
        ])
    );
}

#[test]
fn state_typing_over_a_leaf_node_inserts_inline_content() {
    let state = state_with_doc(doc(vec![
        paragraph_text("a"),
        horizontal_rule(),
        paragraph_text("b"),
    ]));

    let mut tr = state.tr();
    tr.set_selection(Selection::node(3)).unwrap();
    tr.replace_selection_with(text("x")).unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(
        state.doc(),
        &doc(vec![
            paragraph_text("a"),
            paragraph_text("x"),
            paragraph_text("b"),
        ])
    );
    assert_eq!(state.selection(), &Selection::text(5));
}

#[test]
fn state_node_selection_can_be_replaced_with_inline_or_block_nodes() {
    // Replacing an inline node selection with a hard-break keeps content inline.
    let state = state_with_doc(doc(vec![paragraph(vec![
        text("foo"),
        image("one.png"),
        text("bar"),
        image("two.png"),
        text("baz"),
    ])]));

    let mut tr = state.tr();
    tr.set_selection(Selection::node(4)).unwrap();
    tr.replace_selection_with(hard_break()).unwrap();
    let state = state.apply(tr).unwrap();

    assert_eq!(
        state.doc(),
        &doc(vec![paragraph(vec![
            text("foo"),
            hard_break(),
            text("bar"),
            image("two.png"),
            text("baz"),
        ])])
    );
    assert_eq!(state.selection(), &Selection::text(5));

    // Replacing a block node selection with another block node leaves a cursor at its end.
    // Mirrors upstream prosemirror-state/test-selection's "can replace a block selection":
    //   doc(p("abc"), hr(), hr(), blockquote(p("ow"))) → nodeSel(5) → replaceSelectionWith(pre)
    //   expected: doc(p("abc"), pre(), hr(), blockquote(p("ow")))
    //   expected selection.from = 7 (cursor on the next hr — Selection::near walks
    //   into the next selectable block leaf).
    let state = state_with_doc(doc(vec![
        paragraph_text("abc"),
        horizontal_rule(),
        horizontal_rule(),
        schema_basic::blockquote(vec![paragraph_text("ow")]).unwrap(),
    ]));

    let mut tr = state.tr();
    tr.set_selection(Selection::node(5)).unwrap();
    tr.replace_selection_with(code_block("")).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(
        state.doc(),
        &doc(vec![
            paragraph_text("abc"),
            code_block(""),
            horizontal_rule(),
            schema_basic::blockquote(vec![paragraph_text("ow")]).unwrap(),
        ])
    );
    assert_eq!(state.selection().from(state.doc()), 7);

    // Second case: nodeSel the blockquote at pos 8, replace with an empty
    // paragraph (matching PM's `state.nodeSel(8); replaceSelectionWith(p)`).
    //   expected: doc(p("abc"), pre(), hr(), p())
    //   expected selection.from = 9 (text cursor inside the new empty paragraph).
    let mut tr = state.tr();
    tr.set_selection(Selection::node(8)).unwrap();
    tr.replace_selection_with(empty_paragraph()).unwrap();
    let state = state.apply(tr).unwrap();
    assert_eq!(
        state.doc(),
        &doc(vec![
            paragraph_text("abc"),
            code_block(""),
            horizontal_rule(),
            empty_paragraph(),
        ])
    );
    assert_eq!(state.selection().from(state.doc()), 9);
}

#[test]
fn state_plugin_filter_blocks_transaction() {
    let blocker = Plugin::builder("filter")
        .filter_transaction(|tr, _| Ok(tr.meta("reject") != Some(&json!(true))))
        .finish();

    let state = starter_state().reconfigure(vec![blocker]).unwrap();
    let mut accept = state.tr();
    accept.insert_text("Z").unwrap();
    let (next, accepted) = state.apply_transaction(accept).unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(next.doc().text_content(), "Zok");

    let mut reject = state.tr();
    reject.insert_text("Y").unwrap();
    reject.set_meta("reject", json!(true));
    let (same, applied) = state.apply_transaction(reject).unwrap();
    assert!(applied.is_empty());
    assert_eq!(same.doc().text_content(), "ok");
}

#[test]
fn state_plugin_append_transaction_runs_after_user_transaction() {
    let appender = Plugin::builder("append")
        .append_transaction(|trs, _, new_state| {
            if trs
                .last()
                .is_some_and(|tr| tr.meta("append") == Some(&json!(true)))
            {
                let mut follow_up = new_state.tr();
                follow_up.insert_text("A")?;
                Ok(Some(follow_up))
            } else {
                Ok(None)
            }
        })
        .finish();

    let state = starter_state().reconfigure(vec![appender]).unwrap();
    let mut tr = state.tr();
    tr.insert_text("X").unwrap();
    tr.set_meta("append", json!(true));
    let (next, applied) = state.apply_transaction(tr).unwrap();
    assert_eq!(next.doc().text_content(), "XAok");
    assert_eq!(applied.len(), 2);
}

#[test]
fn state_plugin_key_lookups_locate_plugin_state() {
    let counter = Plugin::builder("counter")
        .state_field(
            |_| Ok(json!(0)),
            |transaction, value, _, _| {
                Ok(json!(
                    value.as_u64().unwrap_or(0) + transaction.transform().steps().len() as u64
                ))
            },
        )
        .finish();
    let state = starter_state().reconfigure(vec![counter.clone()]).unwrap();
    assert_eq!(state.plugin_state(counter.key()), Some(&json!(0)));

    let mut tr = state.tr();
    tr.delete(1, 2).unwrap();
    let next = state.apply(tr).unwrap();
    assert_eq!(next.plugin_state(counter.key()), Some(&json!(1)));
}

#[test]
fn state_appended_transactions_chain_through_plugins() {
    // Two appending plugins should each get a chance to emit a follow-up; the
    // resulting transactions slice contains the user transaction plus every
    // appended transaction in firing order.
    let first = Plugin::builder("first")
        .append_transaction(|trs, _, new_state| {
            if trs
                .last()
                .is_some_and(|tr| tr.meta("first-go") == Some(&json!(true)))
                && trs.last().unwrap().meta("first-done") != Some(&json!(true))
            {
                let mut next = new_state.tr();
                next.insert_text("F")?;
                next.set_meta("first-done", json!(true));
                Ok(Some(next))
            } else {
                Ok(None)
            }
        })
        .finish();
    let second = Plugin::builder("second")
        .append_transaction(|trs, _, new_state| {
            if trs
                .last()
                .is_some_and(|tr| tr.meta("first-done") == Some(&json!(true)))
                && trs.last().unwrap().meta("second-done") != Some(&json!(true))
            {
                let mut next = new_state.tr();
                next.insert_text("S")?;
                next.set_meta("second-done", json!(true));
                Ok(Some(next))
            } else {
                Ok(None)
            }
        })
        .finish();

    let state = starter_state().reconfigure(vec![first, second]).unwrap();
    let mut tr = state.tr();
    tr.insert_text("U").unwrap();
    tr.set_meta("first-go", json!(true));
    let (next, applied) = state.apply_transaction(tr).unwrap();
    // Each append uses the new state's selection (which has moved after the
    // previously inserted character), so they accumulate left-to-right.
    assert_eq!(next.doc().text_content(), "UFSok");
    assert_eq!(applied.len(), 3);
}

#[test]
fn state_appended_transaction_carries_root_reference_meta() {
    // Plugins that emit appended transactions get the upstream
    // `appendedTransaction` meta key set automatically, letting downstream
    // consumers identify follow-ups originating from a user transaction.
    let appender = Plugin::builder("append")
        .append_transaction(|trs, _, new_state| {
            if trs
                .last()
                .is_some_and(|tr| tr.meta("trigger") == Some(&json!(true)))
                && trs.last().unwrap().meta("appended-yet") != Some(&json!(true))
            {
                let mut follow = new_state.tr();
                follow.insert_text("Y")?;
                follow.set_meta("appended-yet", json!(true));
                Ok(Some(follow))
            } else {
                Ok(None)
            }
        })
        .finish();

    let state = starter_state().reconfigure(vec![appender]).unwrap();
    let mut tr = state.tr();
    tr.insert_text("X").unwrap();
    tr.set_meta("trigger", json!(true));
    let (_, applied) = state.apply_transaction(tr).unwrap();
    assert_eq!(applied.len(), 2);
    assert!(applied[0].meta(META_APPENDED_TRANSACTION).is_none());
    assert!(applied[1].meta(META_APPENDED_TRANSACTION).is_some());
}

#[test]
fn state_reconfigure_drops_removed_plugin_state_and_initializes_new_plugins() {
    let first = Plugin::builder("first")
        .state_field(|_| Ok(json!("first")), |_, value, _, _| Ok(value.clone()))
        .finish();
    let second = Plugin::builder("second")
        .state_field(|_| Ok(json!("second")), |_, value, _, _| Ok(value.clone()))
        .finish();

    let state = starter_state().reconfigure(vec![first]).unwrap();
    assert_eq!(state.plugin_state("first"), Some(&json!("first")));

    let state = state.reconfigure(vec![second]).unwrap();
    assert_eq!(state.plugin_state("first"), None);
    assert_eq!(state.plugin_state("second"), Some(&json!("second")));
}
