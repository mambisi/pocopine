use serde_json::json;

use pine_richtext::model::{Fragment, Slice};
use pine_richtext::schema_basic;
use pine_richtext::state::{EditorState, EditorStateConfig, Plugin, Selection};

fn starter_state() -> EditorState {
    let schema = schema_basic::schema();
    let doc = schema_basic::doc(vec![
        schema_basic::paragraph(vec![schema_basic::text("hello", Vec::new()).unwrap()]).unwrap(),
    ])
    .unwrap();

    EditorState::create(EditorStateConfig::new(schema, doc)).unwrap()
}

#[test]
fn public_core_chain_builds_transforms_and_updates_state() {
    let state = starter_state();
    let strong = schema_basic::strong().unwrap();
    let mut transaction = state.tr();
    transaction
        .add_mark(1, 6, strong)
        .unwrap()
        .replace(
            6,
            6,
            Slice::new(
                Fragment::from(schema_basic::text("!", Vec::new()).unwrap()),
                0,
                0,
            ),
        )
        .unwrap()
        .set_selection(Selection::text(7))
        .unwrap();

    let state = state.apply(transaction).unwrap();
    assert_eq!(state.doc().text_content(), "hello!");
    assert_eq!(state.selection(), &Selection::text(7));
}

#[test]
fn public_plugin_hooks_filter_and_append_transactions() {
    let append_plugin = Plugin::builder("append-once")
        .state_field(
            |_| Ok(json!(0)),
            |transaction, value, _, _| {
                Ok(json!(
                    value.as_u64().unwrap_or(0) + transaction.transform().steps().len() as u64
                ))
            },
        )
        .append_transaction(|transactions, _, new_state| {
            if transactions
                .iter()
                .any(|transaction| transaction.meta("appended") == Some(&json!(true)))
            {
                return Ok(None);
            }

            let mut appended = new_state.tr();
            appended
                .replace(
                    new_state.doc().content_size() - 1,
                    new_state.doc().content_size() - 1,
                    Slice::new(
                        Fragment::from(schema_basic::text("!", Vec::new()).unwrap()),
                        0,
                        0,
                    ),
                )
                .unwrap()
                .set_meta("appended", json!(true));
            Ok(Some(appended))
        })
        .finish();

    let state = starter_state().reconfigure(vec![append_plugin]).unwrap();
    let mut transaction = state.tr();
    transaction.delete(1, 2).unwrap();

    let (state, transactions) = state.apply_transaction(transaction).unwrap();
    assert_eq!(transactions.len(), 2);
    assert_eq!(state.doc().text_content(), "ello!");
    assert_eq!(state.plugin_state("append-once"), Some(&json!(2)));
}
