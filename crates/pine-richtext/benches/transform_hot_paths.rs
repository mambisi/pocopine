use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

use pine_richtext::model::{Attrs, Fragment, Node, Schema, Slice};
use pine_richtext::schema_basic;
use pine_richtext::transform::Transform;

fn schema() -> Schema {
    schema_basic::schema()
}

fn text(value: &str) -> Node {
    schema_basic::text(value, Vec::new()).unwrap()
}

fn paragraph_with(value: &str) -> Node {
    schema_basic::paragraph(vec![text(value)]).unwrap()
}

fn doc_with_paragraphs(count: usize) -> Node {
    let children = (0..count)
        .map(|i| paragraph_with(&format!("paragraph number {i}")))
        .collect::<Vec<_>>();
    schema_basic::doc(children).unwrap()
}

fn doc_with_nested_blockquote(p_count: usize) -> Node {
    let inner = (0..p_count)
        .map(|i| paragraph_with(&format!("nested paragraph {i}")))
        .collect::<Vec<_>>();
    let bq = schema_basic::blockquote(inner).unwrap();
    schema_basic::doc(vec![bq]).unwrap()
}

fn doc_with_list(item_count: usize) -> Node {
    let items = (0..item_count)
        .map(|i| schema_basic::list_item(vec![paragraph_with(&format!("item {i}"))]).unwrap())
        .collect::<Vec<_>>();
    let list = schema_basic::bullet_list(items).unwrap();
    schema_basic::doc(vec![list]).unwrap()
}

fn bench_set_block_type_paragraph_to_heading(c: &mut Criterion) {
    let schema = schema();
    let doc = doc_with_paragraphs(50);
    let size = doc.content_size();
    c.bench_function("set_block_type/50_paragraphs_to_heading", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            let mut attrs = Attrs::new();
            attrs.insert("level".to_string(), serde_json::json!(2));
            tr.set_block_type(black_box(0), black_box(size), "heading", attrs)
                .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

fn bench_lift_paragraph_from_blockquote(c: &mut Criterion) {
    let schema = schema();
    // 20 nested paragraphs inside a blockquote; lift the 10th out.
    let doc = doc_with_nested_blockquote(20);
    // Position inside the 10th paragraph's text: bq.open(1) + 9 paragraphs of size ~20 + open + offset.
    // Use a position resolver to find a stable point inside paragraph 10.
    // Each paragraph "nested paragraph N" is 1+("nested paragraph N".len())+1 bytes.
    let target_text = "nested paragraph 9";
    // Walk the doc to find pos inside the 10th paragraph's text.
    let mut acc = 1; // open of blockquote
    for i in 0..9 {
        let label = format!("nested paragraph {i}");
        acc += 1 + label.len() + 1; // p_open + text + p_close
    }
    let pos_inside_target = acc + 1 + (target_text.len() / 2);

    c.bench_function("lift/paragraph_from_blockquote_20", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            tr.lift(black_box(pos_inside_target), black_box(pos_inside_target))
                .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

fn bench_lift_paragraph_through_list(c: &mut Criterion) {
    let schema = schema();
    // 15 list items; lift the 8th's paragraph out (walker has to go past
    // bullet_list to find a valid target, exercising the multi-level lift).
    let doc = doc_with_list(15);
    // Walk to find a position inside the 8th list_item's paragraph text.
    let mut acc = 1; // open of bullet_list
    for i in 0..7 {
        let label = format!("item {i}");
        // li (open) + p (open) + text + p (close) + li (close)
        acc += 1 + 1 + label.len() + 1 + 1;
    }
    // Now inside list_item 8: li_open + p_open + text_offset
    let pos_inside_target = acc + 1 + 1 + 3;

    c.bench_function("lift/paragraph_through_list_15", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            tr.lift(black_box(pos_inside_target), black_box(pos_inside_target))
                .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

fn bench_replace_inline_into_block(c: &mut Criterion) {
    let schema = schema();
    let doc = doc_with_paragraphs(50);
    // Slice = a hard_break (inline content) inserted between two top-level
    // paragraphs — exercises fit_inline_slice_inside_block_parent.
    let inline_slice = Slice::new(Fragment::from(schema_basic::hard_break().unwrap()), 0, 0);
    // Position between paragraph 25 and 26.
    let mut acc = 0;
    for i in 0..25 {
        let label = format!("paragraph number {i}");
        acc += 1 + label.len() + 1;
    }

    c.bench_function("replace/inline_into_block_50_paragraphs", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            tr.replace(black_box(acc), black_box(acc), inline_slice.clone())
                .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

fn bench_replace_block_into_textblock(c: &mut Criterion) {
    let schema = schema();
    let doc = doc_with_paragraphs(50);
    // Slice = a flat paragraph inserted in the middle of a textblock —
    // exercises fit_block_slice_inside_textblock.
    let block_slice = Slice::new(Fragment::from(paragraph_with("inserted block")), 0, 0);
    // Position inside the 25th paragraph's text.
    let mut acc = 0;
    for i in 0..25 {
        let label = format!("paragraph number {i}");
        acc += 1 + label.len() + 1;
    }
    let pos_in_text = acc + 1 + 5;

    c.bench_function("replace/block_into_textblock_50_paragraphs", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            tr.replace(
                black_box(pos_in_text),
                black_box(pos_in_text),
                block_slice.clone(),
            )
            .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

fn bench_replace_empty_paragraph_with_heading(c: &mut Criterion) {
    let schema = schema();
    // doc with one empty paragraph; insert a heading-wrapped slice via
    // include_parents=true. Exercises fit_empty_parent_with_wrapped_slice.
    let doc = schema_basic::doc(vec![schema_basic::paragraph(Vec::new()).unwrap()]).unwrap();
    let source = schema_basic::doc(vec![
        schema_basic::heading(1, vec![text("heading text content")]).unwrap(),
    ])
    .unwrap();
    // Slice content of the heading with open ends so the slice carries its
    // heading wrapper.
    let slice = source.slice_with_parents(1, 1 + 20, true).unwrap();

    c.bench_function("replace_range/empty_para_with_heading_slice", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            tr.replace_range(black_box(1), black_box(1), slice.clone())
                .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

fn bench_cross_depth_delete(c: &mut Criterion) {
    let schema = schema();
    // doc(h1("...wo|ah"), bq(p("ah|ha..."))) — exercises fit_cross_depth_empty_slice.
    let h1 = schema_basic::heading(1, vec![text("woah more content")]).unwrap();
    let bq = schema_basic::blockquote(vec![paragraph_with("ahha more content here")]).unwrap();
    let doc = schema_basic::doc(vec![h1, bq]).unwrap();
    // Position inside h1 text (after "wo").
    let from = 1 + 2;
    // Position inside bq's paragraph text (after "ah").
    let h1_size = doc.content().child(0).unwrap().node_size();
    let to = h1_size + 1 + 1 + 2;

    c.bench_function("replace/cross_depth_delete", |b| {
        b.iter(|| {
            let mut tr = Transform::new(schema.clone(), doc.clone());
            tr.replace(black_box(from), black_box(to), Slice::empty())
                .unwrap();
            black_box(tr.doc().clone());
        });
    });
}

criterion_group!(
    benches,
    bench_set_block_type_paragraph_to_heading,
    bench_lift_paragraph_from_blockquote,
    bench_lift_paragraph_through_list,
    bench_replace_inline_into_block,
    bench_replace_block_into_textblock,
    bench_replace_empty_paragraph_with_heading,
    bench_cross_depth_delete,
);
criterion_main!(benches);
