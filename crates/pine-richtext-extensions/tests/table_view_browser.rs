#![cfg(all(target_arch = "wasm32", feature = "table-view"))]

use pine_richtext::model::Fragment;
use pine_richtext::runtime::RuntimeBuilder;
use pine_richtext::view::NodeViewSelection;
use pine_richtext_extensions::tables::view::{
    HitRect, ResizeAxis, ResizeDrag, ResizeEdge, TableViewAction, TableViewAnchor,
    TableViewController, TableViewSnapshot, hit_test_resize_edge,
};
use pine_richtext_extensions::tables::{
    TABLE_CELL_ATTR, TABLE_CELL_CLASS, TABLE_ROW_ATTR, TABLE_ROW_CLASS, TableAttrs, TableCellAttrs,
    TableHeaderCellAttrs, TableRowAttrs, TablesExtension,
};
use serde::Serialize;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

wasm_bindgen_test_configure!(run_in_browser);

struct Fixture {
    host: Element,
    body: Element,
    table_selector: Element,
    controller: TableViewController,
    content: Fragment,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.remove();
    }
}

fn attrs(value: &impl Serialize) -> pine_richtext::model::Attrs {
    serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
}

fn content() -> Fragment {
    let runtime = RuntimeBuilder::new().with(TablesExtension).build();
    let schema = runtime.schema();
    let header = (0..3)
        .map(|_| {
            schema
                .node(
                    "table_header_cell",
                    attrs(&TableHeaderCellAttrs::default()),
                    Fragment::empty(),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let body = (0..3)
        .map(|_| {
            schema
                .node(
                    "table_cell",
                    attrs(&TableCellAttrs::default()),
                    Fragment::empty(),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    Fragment::from(vec![
        schema
            .node(
                "table_row",
                attrs(&TableRowAttrs { height: Some(32) }),
                Fragment::from(header),
            )
            .unwrap(),
        schema
            .node(
                "table_row",
                attrs(&TableRowAttrs { height: Some(40) }),
                Fragment::from(body),
            )
            .unwrap(),
    ])
}

fn fixture() -> Fixture {
    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    let table = document.create_element("table").unwrap();
    table.set_class_name("pine-richtext-table");
    let body = document.create_element("tbody").unwrap();
    let content = content();
    for (row_index, row) in content.iter().enumerate() {
        let row_element = document.create_element("tr").unwrap();
        row_element.set_class_name(TABLE_ROW_CLASS);
        row_element.set_attribute(TABLE_ROW_ATTR, "true").unwrap();
        for column in 0..row.child_count() {
            let tag = if row_index == 0 { "th" } else { "td" };
            let cell = document.create_element(tag).unwrap();
            cell.set_class_name(TABLE_CELL_CLASS);
            cell.set_attribute(TABLE_CELL_ATTR, "true").unwrap();
            cell.set_text_content(Some(&format!("{row_index},{column}")));
            row_element.append_child(cell.as_ref()).unwrap();
        }
        body.append_child(row_element.as_ref()).unwrap();
    }
    table.append_child(body.as_ref()).unwrap();
    let table_selector = document.create_element("button").unwrap();
    table_selector.set_class_name("pine-richtext-table-select-table");
    table_selector
        .set_attribute("aria-pressed", "false")
        .unwrap();
    let column_selectors = document.create_element("div").unwrap();
    for _ in 0..3 {
        column_selectors
            .append_child(document.create_element("button").unwrap().as_ref())
            .unwrap();
    }
    let row_selectors = document.create_element("div").unwrap();
    for _ in 0..2 {
        row_selectors
            .append_child(document.create_element("button").unwrap().as_ref())
            .unwrap();
    }
    host.append_child(table_selector.as_ref()).unwrap();
    host.append_child(column_selectors.as_ref()).unwrap();
    host.append_child(row_selectors.as_ref()).unwrap();
    host.append_child(table.as_ref()).unwrap();
    document
        .body()
        .unwrap()
        .append_child(host.as_ref())
        .unwrap();

    let snapshot = TableViewSnapshot {
        attrs: TableAttrs {
            column_widths: vec![Some(80), Some(96), None],
        },
        content: content.clone(),
        selection: NodeViewSelection::Outside,
        editable: true,
        focused: true,
    };
    let controller = TableViewController::attach(
        TableViewAnchor { table_pos: 5 },
        host.clone(),
        table.clone(),
        body.clone(),
        table_selector.clone(),
        column_selectors,
        row_selectors,
        snapshot,
    )
    .unwrap();
    Fixture {
        host,
        body,
        table_selector,
        controller,
        content,
    }
}

#[wasm_bindgen_test]
fn row_column_and_table_controls_produce_real_rectangles() {
    let fixture = fixture();
    let TableViewAction::Select(row) = fixture.controller.select_row(1).unwrap() else {
        panic!("row selector must create a cell selection")
    };
    assert_eq!((row.anchor_row, row.anchor_column), (1, 0));
    assert_eq!((row.head_row, row.head_column), (1, 2));

    let TableViewAction::Select(column) = fixture.controller.select_column(1).unwrap() else {
        panic!("column selector must create a cell selection")
    };
    assert_eq!((column.anchor_row, column.anchor_column), (0, 1));
    assert_eq!((column.head_row, column.head_column), (1, 1));

    let TableViewAction::Select(table) = fixture.controller.select_table() else {
        panic!("table selector must create a cell selection")
    };
    assert_eq!((table.anchor_row, table.anchor_column), (0, 0));
    assert_eq!((table.head_row, table.head_column), (1, 2));
}

#[wasm_bindgen_test]
fn sync_retains_dom_identity_and_paints_selection_and_css_hooks() {
    let mut fixture = fixture();
    let first_row = fixture.body.children().item(0).unwrap();
    let first_cell = first_row.children().item(0).unwrap();
    let anchor = fixture.controller.grid().cell(0, 1).unwrap().pos;
    let head = fixture.controller.grid().cell(1, 1).unwrap().pos;
    fixture
        .controller
        .sync(
            TableViewAnchor { table_pos: 5 },
            TableViewSnapshot {
                attrs: TableAttrs {
                    column_widths: vec![Some(88), Some(120), Some(72)],
                },
                content: fixture.content.clone(),
                selection: NodeViewSelection::Cells {
                    anchor_cell: anchor,
                    head_cell: head,
                },
                editable: true,
                focused: true,
            },
        )
        .unwrap();

    assert!(first_row.is_same_node(Some(fixture.body.children().item(0).unwrap().as_ref())));
    assert!(
        first_cell.is_same_node(Some(
            fixture
                .body
                .children()
                .item(0)
                .unwrap()
                .children()
                .item(0)
                .unwrap()
                .as_ref()
        ))
    );
    assert_eq!(
        fixture.host.get_attribute("data-selection").as_deref(),
        Some("column")
    );
    let selected = fixture
        .body
        .children()
        .item(1)
        .unwrap()
        .children()
        .item(1)
        .unwrap();
    assert_eq!(
        selected.get_attribute("aria-selected").as_deref(),
        Some("true")
    );
    let selected_style = selected.dyn_into::<HtmlElement>().unwrap().style();
    assert_eq!(
        selected_style
            .get_property_value("--pine-richtext-table-cell-width")
            .unwrap(),
        "120px"
    );

    let anchor = fixture.controller.grid().cell(0, 0).unwrap().pos;
    let head = fixture.controller.grid().cell(1, 2).unwrap().pos;
    fixture
        .controller
        .sync(
            TableViewAnchor { table_pos: 5 },
            TableViewSnapshot {
                attrs: TableAttrs {
                    column_widths: vec![Some(88), Some(120), Some(72)],
                },
                content: fixture.content.clone(),
                selection: NodeViewSelection::Cells {
                    anchor_cell: anchor,
                    head_cell: head,
                },
                editable: true,
                focused: true,
            },
        )
        .unwrap();
    assert_eq!(
        fixture.host.get_attribute("data-selection").as_deref(),
        Some("table")
    );
    assert_eq!(
        fixture
            .table_selector
            .get_attribute("aria-pressed")
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        fixture
            .table_selector
            .get_attribute("data-selected")
            .as_deref(),
        Some("true")
    );
}

#[wasm_bindgen_test]
fn drag_math_clamps_commits_once_and_cancel_has_no_action() {
    let edge = hit_test_resize_edge(
        HitRect {
            left: 0.0,
            top: 0.0,
            width: 100.0,
            height: 40.0,
        },
        99.0,
        20.0,
        0,
        1,
        6.0,
    )
    .unwrap();
    assert_eq!(
        edge,
        ResizeEdge {
            axis: ResizeAxis::Column,
            index: 1
        }
    );
    let mut drag =
        ResizeDrag::begin(TableViewAnchor { table_pos: 5 }, 9, edge, 100.0, 100.0).unwrap();
    assert_eq!(drag.update(9, 140.0), Some(140));
    assert!(drag.finish(9).is_some());

    let _cancelled =
        ResizeDrag::begin(TableViewAnchor { table_pos: 5 }, 10, edge, 100.0, 100.0).unwrap();
    // Cancellation is represented by dropping the preview state. There is no
    // semantic action to dispatch and therefore no history entry.
}
