#![cfg(feature = "tables")]

use std::sync::Arc;

use pine_richtext::commands::BoxedCommand;
use pine_richtext::history::{history_plugin, undo};
use pine_richtext::model::{Attrs, Fragment, Node, Schema};
use pine_richtext::runtime::{EditorRuntime, RuntimeBuilder};
use pine_richtext::state::{EditorState, EditorStateConfig, Selection, Transaction};
use pine_richtext::transform::{Mapping, StepMap};
use pine_richtext_extensions::tables::{
    MAX_COLUMN_WIDTH, MAX_ROW_HEIGHT, MIN_COLUMN_WIDTH, MIN_ROW_HEIGHT, TableAlignment,
    TableCellAttrs, TableHeaderCellAttrs, TableMap, TableMapError, TableRowAttrs, TablesExtension,
    delete_column, delete_row, go_to_next_cell, go_to_previous_cell, insert_column_after,
    insert_column_before, insert_row_after, insert_row_before, insert_table, set_cell_alignment_at,
    set_column_width_at, set_row_height_at,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

fn runtime() -> Arc<EditorRuntime> {
    RuntimeBuilder::new().with(TablesExtension).build()
}

fn attrs(value: impl serde::Serialize) -> Attrs {
    match serde_json::to_value(value).expect("serialize attrs") {
        Value::Object(object) => object.into_iter().collect(),
        _ => panic!("node attrs must serialize as an object"),
    }
}

fn empty_cell(schema: &Schema, header: bool) -> Node {
    schema
        .node(
            if header {
                "table_header_cell"
            } else {
                "table_cell"
            },
            if header {
                attrs(TableHeaderCellAttrs::default())
            } else {
                attrs(TableCellAttrs::default())
            },
            Fragment::empty(),
        )
        .expect("cell")
}

fn table_node(schema: &Schema, rows: usize, columns: usize) -> Node {
    let rows = (0..rows)
        .map(|row| {
            let cells = (0..columns)
                .map(|_| empty_cell(schema, row == 0))
                .collect::<Vec<_>>();
            schema
                .node(
                    "table_row",
                    attrs(TableRowAttrs::default()),
                    Fragment::from(cells),
                )
                .expect("row")
        })
        .collect::<Vec<_>>();
    schema
        .node("table", Attrs::new(), Fragment::from(rows))
        .expect("table")
}

fn table_doc(schema: &Schema, rows: usize, columns: usize) -> Node {
    schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(table_node(schema, rows, columns)),
        )
        .expect("doc")
}

fn labeled_table_doc(schema: &Schema, rows: usize, columns: usize) -> Node {
    let rows = (0..rows)
        .map(|row| {
            let cells = (0..columns)
                .map(|column| {
                    let text = schema
                        .text(format!("{row},{column}"), Vec::new())
                        .expect("cell text");
                    schema
                        .node(
                            if row == 0 {
                                "table_header_cell"
                            } else {
                                "table_cell"
                            },
                            if row == 0 {
                                attrs(TableHeaderCellAttrs::default())
                            } else {
                                attrs(TableCellAttrs::default())
                            },
                            Fragment::from(text),
                        )
                        .expect("labeled cell")
                })
                .collect::<Vec<_>>();
            schema
                .node(
                    "table_row",
                    attrs(TableRowAttrs::default()),
                    Fragment::from(cells),
                )
                .expect("labeled row")
        })
        .collect::<Vec<_>>();
    let table = schema
        .node("table", Attrs::new(), Fragment::from(rows))
        .expect("labeled table");
    schema
        .node("doc", Attrs::new(), Fragment::from(table))
        .expect("labeled table doc")
}

fn first_table(doc: &Node) -> (usize, &Node) {
    let mut found_pos = None;
    doc.descendants(|node, pos| {
        if found_pos.is_none() && node.type_name() == "table" {
            found_pos = Some(pos);
        }
    });
    let pos = found_pos.expect("document table");
    (
        pos,
        doc.node_at(pos)
            .expect("valid table position")
            .expect("table node"),
    )
}

fn map(doc: &Node) -> TableMap {
    let (pos, table) = first_table(doc);
    TableMap::new(table, pos).expect("valid table map")
}

fn state_at(schema: &Schema, doc: Node, row: usize, column: usize) -> EditorState {
    let table_map = map(&doc);
    let selection = Selection::text(table_map.cell(row, column).expect("selected cell").pos + 1);
    EditorState::create(EditorStateConfig::new(schema.clone(), doc).selection(selection))
        .expect("editor state")
}

fn run(state: &EditorState, command: BoxedCommand) -> (EditorState, Transaction) {
    let transaction = command.apply(state).expect("command applies");
    let next = state.apply(transaction.clone()).expect("apply transaction");
    (next, transaction)
}

fn decoded<A: DeserializeOwned>(node: &Node) -> A {
    serde_json::from_value(serde_json::to_value(node.attrs()).expect("encode attrs"))
        .expect("decode attrs")
}

#[test]
fn gfm_tables_round_trip_header_body_and_alignment() {
    let runtime = runtime();
    let source = concat!(
        "| Name | Count | Notes |\n",
        "| :--- | ---: | :---: |\n",
        "| Alpha | 1 | first |\n",
        "| Beta | 2 | second |\n",
    );
    let doc = runtime
        .markdown_parser()
        .parse(source, runtime.schema())
        .expect("parse GFM table");
    let table_map = map(&doc);
    assert_eq!((table_map.height(), table_map.width()), (3, 3));

    let (_, table) = first_table(&doc);
    let expected = [
        Some(TableAlignment::Left),
        Some(TableAlignment::Right),
        Some(TableAlignment::Center),
    ];
    for (row_index, row) in table.content().iter().enumerate() {
        for (column, cell) in row.content().iter().enumerate() {
            assert_eq!(
                cell.type_name(),
                if row_index == 0 {
                    "table_header_cell"
                } else {
                    "table_cell"
                }
            );
            let alignment = cell
                .attrs()
                .get("alignment")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok());
            assert_eq!(alignment, expected[column]);
        }
    }

    let output = runtime
        .markdown_serializer()
        .serialize(&doc)
        .expect("serialize GFM table");
    let reparsed = runtime
        .markdown_parser()
        .parse(&output, runtime.schema())
        .expect("reparse serialized table");
    assert_eq!((map(&reparsed).height(), map(&reparsed).width()), (3, 3));
    assert!(output.contains("Alpha"), "serialized markdown: {output}");
    assert!(output.contains(":---"), "serialized markdown: {output}");
}

#[test]
fn structural_commands_keep_the_table_rectangular_and_header_canonical() {
    let runtime = runtime();
    let schema = runtime.schema();
    let state = state_at(schema, table_doc(schema, 2, 2), 0, 0);

    let (state, tr) = run(&state, insert_row_before());
    assert_eq!(tr.transform().steps().len(), 1);
    let table_map = map(state.doc());
    assert_eq!((table_map.height(), table_map.width()), (3, 2));
    let (_, table) = first_table(state.doc());
    assert!(
        table
            .child(0)
            .expect("new header")
            .content()
            .iter()
            .all(|cell| cell.type_name() == "table_header_cell")
    );
    assert!(
        table
            .child(1)
            .expect("demoted old header")
            .content()
            .iter()
            .all(|cell| cell.type_name() == "table_cell")
    );

    let (state, _) = run(&state, delete_row());
    let (_, table) = first_table(state.doc());
    assert_eq!(table.child_count(), 2);
    assert!(
        table
            .child(0)
            .expect("promoted header")
            .content()
            .iter()
            .all(|cell| cell.type_name() == "table_header_cell")
    );

    let (state, _) = run(&state, insert_column_before());
    assert_eq!(
        (map(state.doc()).height(), map(state.doc()).width()),
        (2, 3)
    );
    let (state, _) = run(&state, insert_column_after());
    assert_eq!(
        (map(state.doc()).height(), map(state.doc()).width()),
        (2, 4)
    );
    let (state, _) = run(&state, delete_column());
    assert_eq!(
        (map(state.doc()).height(), map(state.doc()).width()),
        (2, 3)
    );
    let (state, _) = run(&state, insert_row_after());
    assert_eq!(
        (map(state.doc()).height(), map(state.doc()).width()),
        (3, 3)
    );
}

#[test]
fn dimensions_clamp_alignment_targets_and_markdown_drops_pixel_metadata() {
    let runtime = runtime();
    let schema = runtime.schema();
    let state = state_at(schema, table_doc(schema, 2, 2), 1, 1);

    let (state, width_tr) = run(&state, set_column_width_at(1, 1));
    assert_eq!(width_tr.transform().steps().len(), 1);
    let (_, table) = first_table(state.doc());
    let widths = table
        .attrs()
        .get("column_widths")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<Option<u32>>>(value).ok())
        .expect("column widths");
    assert_eq!(widths, vec![None, Some(MIN_COLUMN_WIDTH)]);
    assert!(set_column_width_at(1, 1).apply(&state).is_none());

    let (state, _) = run(&state, set_row_height_at(1, u32::MAX));
    let (_, table) = first_table(state.doc());
    let row: TableRowAttrs = decoded(table.child(1).expect("body row"));
    assert_eq!(row.height, Some(MAX_ROW_HEIGHT));
    assert!(set_row_height_at(1, u32::MAX).apply(&state).is_none());

    let (state, _) = run(
        &state,
        set_cell_alignment_at(1, 0, Some(TableAlignment::Right)),
    );
    let (_, table) = first_table(state.doc());
    let cell: TableCellAttrs = decoded(
        table
            .child(1)
            .and_then(|row| row.child(0))
            .expect("target cell"),
    );
    assert_eq!(cell.alignment, Some(TableAlignment::Right));

    let markdown = runtime
        .markdown_serializer()
        .serialize(state.doc())
        .expect("serialize table");
    let reparsed = runtime
        .markdown_parser()
        .parse(&markdown, schema)
        .expect("reparse table");
    let (_, reparsed_table) = first_table(&reparsed);
    let widths = reparsed_table
        .attrs()
        .get("column_widths")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<Option<u32>>>(value).ok())
        .expect("default widths");
    assert!(widths.is_empty());
    for row in reparsed_table.content().iter() {
        let attrs: TableRowAttrs = decoded(row);
        assert_eq!(attrs.height, None);
    }

    let (state, _) = run(&state, set_column_width_at(0, u32::MAX));
    let (_, table) = first_table(state.doc());
    let widths = table
        .attrs()
        .get("column_widths")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<Option<u32>>>(value).ok())
        .expect("column widths");
    assert_eq!(widths[0], Some(MAX_COLUMN_WIDTH));

    let (state, _) = run(&state, set_row_height_at(0, 0));
    let (_, table) = first_table(state.doc());
    let row: TableRowAttrs = decoded(table.child(0).expect("header row"));
    assert_eq!(row.height, Some(MIN_ROW_HEIGHT));
}

#[test]
fn tab_moves_in_row_major_order_and_appends_after_the_last_cell() {
    let runtime = runtime();
    let schema = runtime.schema();
    let state = state_at(schema, table_doc(schema, 2, 2), 0, 0);

    assert!(go_to_previous_cell().apply(&state).is_none());
    let (state, tr) = run(&state, go_to_next_cell());
    assert!(tr.transform().steps().is_empty());
    let table_map = map(state.doc());
    assert_eq!(
        state.selection(),
        &Selection::text(table_map.cell(0, 1).expect("next cell").pos + 1)
    );

    let state = state_at(schema, state.doc().clone(), 1, 1);
    let (state, tr) = run(&state, go_to_next_cell());
    assert_eq!(tr.transform().steps().len(), 1);
    let table_map = map(state.doc());
    assert_eq!((table_map.height(), table_map.width()), (3, 2));
    assert_eq!(
        state.selection(),
        &Selection::text(table_map.cell(2, 0).expect("appended row first cell").pos + 1)
    );

    let (_, backwards) = run(&state, go_to_previous_cell());
    assert!(backwards.transform().steps().is_empty());
}

#[test]
fn deleting_the_only_row_or_column_removes_the_table_without_invalidating_the_doc() {
    let runtime = runtime();
    let schema = runtime.schema();
    for command in [delete_row(), delete_column()] {
        let state = state_at(schema, table_doc(schema, 1, 1), 0, 0);
        let (state, transaction) = run(&state, command);
        assert_eq!(transaction.transform().steps().len(), 1);
        assert!(
            state
                .doc()
                .content()
                .iter()
                .all(|node| node.type_name() != "table"),
            "the last table dimension should remove the table"
        );
    }
}

#[test]
fn insert_table_and_runtime_contributions_are_available_without_a_view() {
    let runtime = runtime();
    let schema = runtime.schema();
    let paragraph = schema
        .node("paragraph", Attrs::new(), Fragment::empty())
        .expect("paragraph");
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(paragraph))
        .expect("doc");
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone(), doc).selection(Selection::text(1)),
    )
    .expect("state");
    let (state, tr) = run(&state, insert_table(3, 4));
    assert_eq!(tr.transform().steps().len(), 1);
    assert_eq!(
        (map(state.doc()).height(), map(state.doc()).width()),
        (3, 4)
    );

    for name in [
        "insert_table",
        "insert_row_above",
        "insert_row_below",
        "insert_column_left",
        "insert_column_right",
        "delete_table",
        "set_cell_alignment",
        "set_column_width",
        "set_row_height",
    ] {
        assert!(runtime.named_command(name).is_some(), "missing `{name}`");
    }
    let keys = runtime
        .merged_keymap_factories()
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"Tab"));
    assert!(keys.contains(&"Shift-Tab"));
}

#[test]
fn table_map_rejects_ragged_and_noncanonical_cell_kinds() {
    let runtime = runtime();
    let schema = runtime.schema();

    let first = schema
        .node(
            "table_row",
            Attrs::new(),
            Fragment::from(vec![empty_cell(schema, true), empty_cell(schema, true)]),
        )
        .expect("first row");
    let second = schema
        .node(
            "table_row",
            Attrs::new(),
            Fragment::from(empty_cell(schema, false)),
        )
        .expect("second row");
    let ragged = schema
        .node("table", Attrs::new(), Fragment::from(vec![first, second]))
        .expect("ragged table is schema-valid");
    assert!(matches!(
        TableMap::new(&ragged, 0),
        Err(TableMapError::RaggedRow {
            row: 1,
            expected: 2,
            found: 1,
        })
    ));

    let non_header = schema
        .node(
            "table_row",
            Attrs::new(),
            Fragment::from(empty_cell(schema, false)),
        )
        .expect("row");
    let invalid = schema
        .node("table", Attrs::new(), Fragment::from(non_header))
        .expect("table");
    assert!(matches!(
        TableMap::new(&invalid, 0),
        Err(TableMapError::InvalidCellType {
            row: 0,
            column: 0,
            expected: "table_header_cell",
            ..
        })
    ));
}

#[test]
fn table_map_property_loop_covers_rectangles_and_positions() {
    let runtime = runtime();
    let schema = runtime.schema();
    for rows in 1..=8 {
        for columns in 1..=8 {
            let table = table_node(schema, rows, columns);
            let table_map = TableMap::new(&table, 7).expect("rectangular map");
            assert_eq!((table_map.height(), table_map.width()), (rows, columns));

            let mut positions = Vec::with_capacity(rows * columns);
            for row in 0..rows {
                for column in 0..columns {
                    positions.push(table_map.cell(row, column).expect("mapped cell").pos);
                }
            }
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            for (index, pos) in positions.iter().enumerate() {
                let cell = table_map.find_cell(pos + 1).expect("position lookup");
                assert_eq!((cell.top, cell.left), (index / columns, index % columns));
            }

            let first = table_map.cell(0, 0).expect("first");
            let last = table_map.cell(rows - 1, columns - 1).expect("last");
            let rect = table_map
                .rect_between(first.pos + 1, last.pos + 1)
                .expect("full rectangle");
            assert_eq!(
                (rect.left, rect.top, rect.right, rect.bottom),
                (0, 0, columns, rows)
            );
            assert_eq!(
                table_map.cells_in_rect(rect).expect("rectangle cells"),
                positions
            );
        }
    }
}

#[test]
fn cell_selection_is_rectangular_non_empty_and_json_round_trips() {
    let runtime = runtime();
    let schema = runtime.schema();
    let doc = labeled_table_doc(schema, 3, 4);
    let table_map = map(&doc);
    let anchor = table_map.cell(2, 3).expect("anchor").pos;
    let head = table_map.cell(0, 1).expect("head").pos;
    let selection = Selection::cells(anchor, head);
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone(), doc.clone()).selection(selection.clone()),
    )
    .expect("valid cell selection");

    assert!(!selection.is_empty(&doc));
    let rect = selection
        .cell_rect(&doc, schema)
        .expect("resolve rectangle")
        .expect("cell rectangle");
    assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (1, 0, 4, 3));
    assert_eq!(selection.ranges(&doc, schema).unwrap().len(), 9);
    assert_eq!(selection.cell_positions(&doc, schema).unwrap().len(), 9);

    let crossed = Selection::cells(
        table_map.cell(2, 1).unwrap().pos,
        table_map.cell(0, 3).unwrap().pos,
    );
    assert_eq!(crossed.from(&doc), table_map.cell(0, 1).unwrap().pos);
    let bottom_right = table_map.cell(2, 3).unwrap();
    assert_eq!(
        crossed.to(&doc),
        bottom_right.pos + doc.node_at(bottom_right.pos).unwrap().unwrap().node_size()
    );

    let encoded = state.to_json().expect("encode state");
    assert_eq!(encoded["selection"]["type"], "cells");
    let decoded =
        EditorState::from_json(schema.clone(), Vec::new(), encoded).expect("decode state");
    assert_eq!(decoded.selection(), &selection);
}

#[test]
fn cell_selection_rejects_non_cells_ragged_tables_and_cross_table_endpoints() {
    let runtime = runtime();
    let schema = runtime.schema();
    let doc = labeled_table_doc(schema, 2, 2);
    let table_map = map(&doc);
    let cell = table_map.cell(0, 0).unwrap().pos;
    assert!(
        Selection::cells(cell + 1, cell)
            .validate(&doc, schema)
            .is_err()
    );

    let first = table_node(schema, 2, 2);
    let second = table_node(schema, 2, 2);
    let two_tables = schema
        .node("doc", Attrs::new(), Fragment::from(vec![first, second]))
        .unwrap();
    let mut tables = Vec::new();
    two_tables.descendants(|node, pos| {
        if node.type_name() == "table" {
            tables.push((pos, node.clone()));
        }
    });
    let first_map = TableMap::new(&tables[0].1, tables[0].0).unwrap();
    let second_map = TableMap::new(&tables[1].1, tables[1].0).unwrap();
    assert!(
        Selection::cells(
            first_map.cell(0, 0).unwrap().pos,
            second_map.cell(0, 0).unwrap().pos,
        )
        .validate(&two_tables, schema)
        .is_err()
    );

    let ragged = schema
        .node(
            "table",
            Attrs::new(),
            Fragment::from(vec![
                schema
                    .node(
                        "table_row",
                        attrs(TableRowAttrs::default()),
                        Fragment::from(vec![empty_cell(schema, true), empty_cell(schema, true)]),
                    )
                    .unwrap(),
                schema
                    .node(
                        "table_row",
                        attrs(TableRowAttrs::default()),
                        Fragment::from(empty_cell(schema, false)),
                    )
                    .unwrap(),
            ]),
        )
        .unwrap();
    let ragged_doc = schema
        .node("doc", Attrs::new(), Fragment::from(ragged.clone()))
        .unwrap();
    let ragged_map = TableMap::new(&ragged, 0);
    assert!(ragged_map.is_err());
    let first_cell_pos = 2;
    assert!(
        Selection::cells(first_cell_pos, first_cell_pos)
            .validate(&ragged_doc, schema)
            .is_err()
    );
}

#[test]
fn cell_selection_maps_as_cells_and_bookmark_resolves_after_prefix_insert() {
    let runtime = runtime();
    let schema = runtime.schema();
    let doc = labeled_table_doc(schema, 2, 3);
    let table_map = map(&doc);
    let selection = Selection::cells(
        table_map.cell(0, 1).unwrap().pos,
        table_map.cell(1, 2).unwrap().pos,
    );
    let paragraph = schema
        .node("paragraph", Attrs::new(), Fragment::empty())
        .unwrap();
    let shift = paragraph.node_size();
    let mapping = Mapping {
        maps: vec![StepMap::single(0, 0, shift)],
        mirrors: Vec::new(),
    };
    let mapped = selection.map(&mapping);
    let prefixed = schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![paragraph, doc.child(0).unwrap().clone()]),
        )
        .unwrap();
    mapped
        .validate(&prefixed, schema)
        .expect("mapped cells stay valid");
    assert_eq!(
        mapped.bookmark().resolve(&prefixed, schema).unwrap(),
        mapped
    );
}

#[test]
fn history_restores_a_rectangular_cell_selection_after_undo() {
    let runtime = runtime();
    let schema = runtime.schema();
    let doc = labeled_table_doc(schema, 2, 2);
    let table_map = map(&doc);
    let selection = Selection::cells(
        table_map.cell(0, 0).unwrap().pos,
        table_map.cell(1, 1).unwrap().pos,
    );
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone(), doc)
            .selection(selection.clone())
            .plugins(vec![history_plugin()]),
    )
    .unwrap();
    let paragraph = schema
        .node("paragraph", Attrs::new(), Fragment::empty())
        .unwrap();
    let mut tr = state.tr();
    tr.insert(0, Fragment::from(paragraph)).unwrap();
    let edited = state.apply(tr).unwrap();
    assert!(edited.selection().is_cells());

    let undo_tr = undo().apply(&edited).expect("undo applies");
    let restored = edited.apply(undo_tr).expect("undo state");
    assert_eq!(restored.selection(), &selection);
    restored
        .selection()
        .validate(restored.doc(), schema)
        .expect("history bookmark resolves to semantic cells");
}

#[test]
fn deleting_a_cell_rectangle_clears_only_selected_cell_contents() {
    let runtime = runtime();
    let schema = runtime.schema();
    let doc = labeled_table_doc(schema, 3, 3);
    let table_map = map(&doc);
    let selection = Selection::cells(
        table_map.cell(0, 1).unwrap().pos,
        table_map.cell(2, 1).unwrap().pos,
    );
    let state =
        EditorState::create(EditorStateConfig::new(schema.clone(), doc).selection(selection))
            .unwrap();
    let mut tr = state.tr();
    tr.delete_selection().expect("clear selected cells");
    let next = state.apply(tr).expect("apply clear");
    let (_, table) = first_table(next.doc());
    for (row, table_row) in table.content().iter().enumerate() {
        assert_eq!(table_row.child(1).unwrap().text_content(), "");
        assert_eq!(
            table_row.child(0).unwrap().text_content(),
            format!("{row},0")
        );
        assert_eq!(
            table_row.child(2).unwrap().text_content(),
            format!("{row},2")
        );
    }
    assert!(next.selection().is_cells());
    next.selection()
        .validate(next.doc(), schema)
        .expect("cleared selection stays semantic");
}

#[test]
fn toggle_mark_applies_per_cell_without_touching_cells_between_linear_endpoints() {
    let runtime = runtime();
    let schema = runtime.schema();
    let doc = labeled_table_doc(schema, 3, 3);
    let table_map = map(&doc);
    let selection = Selection::cells(
        table_map.cell(0, 1).unwrap().pos,
        table_map.cell(2, 1).unwrap().pos,
    );
    let state =
        EditorState::create(EditorStateConfig::new(schema.clone(), doc).selection(selection))
            .unwrap();
    let strong = schema.mark("strong", Attrs::new()).unwrap();
    let (next, _) = run(&state, pine_richtext::commands::toggle_mark(strong.clone()));
    let (_, table) = first_table(next.doc());
    for row in table.content().iter() {
        for column in 0..3 {
            let text = row.child(column).unwrap().child(0).unwrap();
            assert_eq!(
                text.marks().iter().any(|mark| mark == &strong),
                column == 1,
                "only the selected column should be marked"
            );
        }
    }
}

#[test]
fn rectangular_slice_paste_replaces_matching_cells_without_replacing_table_shells() {
    let runtime = runtime();
    let schema = runtime.schema();
    let source_doc = labeled_table_doc(schema, 2, 2);
    let source_map = map(&source_doc);
    let source_selection = Selection::cells(
        source_map.cell(0, 0).unwrap().pos,
        source_map.cell(1, 1).unwrap().pos,
    );
    let slice = source_selection
        .content(&source_doc, schema)
        .expect("rectangular slice");

    let target_doc = labeled_table_doc(schema, 3, 3);
    let target_map = map(&target_doc);
    let target_selection = Selection::cells(
        target_map.cell(1, 1).unwrap().pos,
        target_map.cell(2, 2).unwrap().pos,
    );
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone(), target_doc).selection(target_selection),
    )
    .unwrap();
    let mut tr = state.tr();
    tr.replace_selection(slice).expect("paste rectangle");
    let next = state.apply(tr).unwrap();
    let (_, table) = first_table(next.doc());
    assert_eq!(
        table.child(1).unwrap().child(1).unwrap().text_content(),
        "0,0"
    );
    assert_eq!(
        table.child(1).unwrap().child(2).unwrap().text_content(),
        "0,1"
    );
    assert_eq!(
        table.child(2).unwrap().child(1).unwrap().text_content(),
        "1,0"
    );
    assert_eq!(
        table.child(2).unwrap().child(2).unwrap().text_content(),
        "1,1"
    );
    assert_eq!(
        table.child(0).unwrap().child(0).unwrap().text_content(),
        "0,0"
    );
    assert!(next.selection().is_cells());
}

#[test]
fn cell_selection_property_loop_returns_only_cells_in_each_rectangle() {
    let runtime = runtime();
    let schema = runtime.schema();
    for rows in 1..=5 {
        for columns in 1..=5 {
            let doc = table_doc(schema, rows, columns);
            let table_map = map(&doc);
            for top in 0..rows {
                for left in 0..columns {
                    for bottom in top..rows {
                        for right in left..columns {
                            let selection = Selection::cells(
                                table_map.cell(bottom, right).unwrap().pos,
                                table_map.cell(top, left).unwrap().pos,
                            );
                            let positions = selection.cell_positions(&doc, schema).unwrap();
                            assert_eq!(positions.len(), (bottom - top + 1) * (right - left + 1));
                            let mut expected = Vec::new();
                            for row in top..=bottom {
                                for column in left..=right {
                                    expected.push(table_map.cell(row, column).unwrap().pos);
                                }
                            }
                            assert_eq!(positions, expected);
                            assert_eq!(
                                selection.ranges(&doc, schema).unwrap().len(),
                                expected.len()
                            );
                        }
                    }
                }
            }
        }
    }
}
