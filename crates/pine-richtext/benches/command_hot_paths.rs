use std::hint::black_box;
use std::time::{Duration, Instant};

use pine_richtext::commands::{
    chain_commands, delete_selection, join_backward, join_forward, lift_empty_block,
    select_node_backward, select_node_forward, split_block, split_list_item,
};
use pine_richtext::extension::RichTextExtension;
use pine_richtext::model::{Attrs, Fragment, MarkPolicy, Node, NodeSpec, Schema, Whitespace};
use pine_richtext::runtime::RuntimeBuilder;
use pine_richtext::state::{EditorState, EditorStateConfig, Selection, Transaction};
use pine_richtext::transform::{can_split_into, Transform, TypeAfter};
use serde_json::json;

struct KeepNoteSchemaExtension;

impl RichTextExtension for KeepNoteSchemaExtension {
    fn name(&self) -> &str {
        "core_nodes"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("doc").content("title block*"),
            NodeSpec::new("title")
                .group("title")
                .content("inline*")
                .marks(MarkPolicy::None)
                .defining(),
            NodeSpec::new("paragraph").group("block").content("inline*"),
            NodeSpec::new("blockquote")
                .group("block")
                .content("block+")
                .defining(),
            NodeSpec::new("horizontal_rule").group("block").atom(),
            NodeSpec::new("heading")
                .group("block")
                .content("inline*")
                .defining()
                .attr("level", json!(1)),
            NodeSpec::new("code_block")
                .group("block")
                .content("text*")
                .marks(MarkPolicy::None)
                .code()
                .whitespace(Whitespace::Pre)
                .defining(),
        ]
    }
}

fn keep_schema() -> Schema {
    RuntimeBuilder::new()
        .name("keep-bench")
        .with(KeepNoteSchemaExtension)
        .build()
        .schema()
        .clone()
}

fn text(schema: &Schema, value: &str) -> Node {
    schema.text(value.to_string(), Vec::new()).unwrap()
}

fn textblock(schema: &Schema, name: &str, value: &str) -> Node {
    let mut attrs = Attrs::new();
    if name == "heading" {
        attrs.insert("level".to_string(), json!(1));
    }
    schema
        .node(name, attrs, Fragment::from(text(schema, value)))
        .unwrap()
}

fn keep_title_only_state() -> EditorState {
    let schema = keep_schema();
    let title = textblock(&schema, "title", "Google");
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(title))
        .unwrap();
    state_at_doc_pos(schema, doc, 7)
}

fn keep_heading_state() -> EditorState {
    let schema = keep_schema();
    let title = textblock(&schema, "title", "Google");
    let heading = textblock(&schema, "heading", "FSDA");
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(vec![title, heading]))
        .unwrap();
    let heading_end = text_end_inside_top_level_child(&doc, 1);
    state_at_doc_pos(schema, doc, heading_end)
}

fn keep_paragraph_state(value: &str, cursor_offset: usize) -> EditorState {
    let schema = keep_schema();
    let title = textblock(&schema, "title", "Google");
    let paragraph = textblock(&schema, "paragraph", value);
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(vec![title, paragraph]))
        .unwrap();
    let paragraph_start = (0..1)
        .map(|index| doc.content().child(index).unwrap().node_size())
        .sum::<usize>()
        + 1;
    state_at_doc_pos(schema, doc, paragraph_start + cursor_offset)
}

fn state_at_doc_pos(schema: Schema, doc: Node, pos: usize) -> EditorState {
    EditorState::create(EditorStateConfig::new(schema, doc).selection(Selection::text(pos)))
        .unwrap()
}

fn text_end_inside_top_level_child(doc: &Node, child_index: usize) -> usize {
    let before = (0..child_index)
        .map(|index| doc.content().child(index).unwrap().node_size())
        .sum::<usize>();
    let child = doc.content().child(child_index).unwrap();
    before + 1 + child.text_content().len()
}

fn paragraph_after() -> [Option<TypeAfter>; 1] {
    [Some(TypeAfter::new("paragraph"))]
}

fn legacy_default_textblock_after_match_type(
    doc: &Node,
    pos: usize,
    schema: &Schema,
) -> Option<String> {
    let resolved = doc.resolve(pos).ok()?;
    if resolved.depth() == 0 {
        return None;
    }

    let parent = resolved.parent();
    let parent_type = schema.node_type(parent.type_name()).ok()?;
    if !parent_type.inline_content(schema) || resolved.parent_offset() != parent.content_size() {
        return None;
    }

    let grand_depth = resolved.depth().checked_sub(1)?;
    let grand = resolved.node(grand_depth)?;
    let index_after = resolved.index_after(grand_depth)?;
    let content_match = grand.content_match_at(index_after, schema).ok()?;

    for name in schema.node_type_names() {
        let Ok(candidate) = schema.node_type(name) else {
            continue;
        };
        if candidate.is_inline() || !candidate.inline_content(schema) {
            continue;
        }
        if content_match.match_type(candidate).is_some() {
            if candidate.name() == parent.type_name() {
                return None;
            }
            return Some(candidate.name().to_string());
        }
    }
    None
}

fn current_default_textblock_after_can_replace(
    doc: &Node,
    pos: usize,
    schema: &Schema,
) -> Option<String> {
    let resolved = doc.resolve(pos).ok()?;
    if resolved.depth() == 0 {
        return None;
    }

    let parent = resolved.parent();
    let parent_type = schema.node_type(parent.type_name()).ok()?;
    if !parent_type.inline_content(schema) || resolved.parent_offset() != parent.content_size() {
        return None;
    }

    let grand_depth = resolved.depth().checked_sub(1)?;
    let grand = resolved.node(grand_depth)?;
    let index_after = resolved.index_after(grand_depth)?;
    grand
        .content_match_at(index_after, schema)
        .ok()?
        .default_textblock_type()
        .map(|node_type| node_type.name().to_string())
}

fn can_replace_textblock_candidate(
    grand: &Node,
    index_after: usize,
    name: &str,
    schema: &Schema,
) -> bool {
    let Ok(candidate) = schema.node_type(name) else {
        return false;
    };
    if candidate.is_inline() || !candidate.inline_content(schema) {
        return false;
    }
    grand.can_replace_with(index_after, index_after, candidate.name(), &[], schema)
}

fn match_type_candidate(doc: &Node, pos: usize, name: &str, schema: &Schema) -> Option<bool> {
    let resolved = doc.resolve(pos).ok()?;
    let grand_depth = resolved.depth().checked_sub(1)?;
    let grand = resolved.node(grand_depth)?;
    let index_after = resolved.index_after(grand_depth)?;
    let content_match = grand.content_match_at(index_after, schema).ok()?;
    let candidate = schema.node_type(name).ok()?;
    Some(content_match.match_type(candidate).is_some())
}

fn can_replace_candidate(doc: &Node, pos: usize, name: &str, schema: &Schema) -> Option<bool> {
    let resolved = doc.resolve(pos).ok()?;
    let grand_depth = resolved.depth().checked_sub(1)?;
    let grand = resolved.node(grand_depth)?;
    let index_after = resolved.index_after(grand_depth)?;
    Some(can_replace_textblock_candidate(
        grand,
        index_after,
        name,
        schema,
    ))
}

fn legacy_split_block_apply(state: &EditorState) -> Option<Transaction> {
    let sel = state.selection();
    let from = sel.from(state.doc());
    let to = sel.to(state.doc());
    let resolved = state.doc().resolve(from).ok()?;
    if resolved.depth() == 0 {
        return None;
    }
    let mut tr = state.tr();
    if from < to {
        tr.delete(from, to).ok()?;
    }
    let pos = tr.selection().map(|s| s.from(tr.doc())).unwrap_or(from);
    if let Some(type_name) =
        legacy_default_textblock_after_match_type(tr.doc(), pos, state.schema())
    {
        let types_after = [Some(TypeAfter::new(type_name))];
        if !can_split_into(tr.doc(), pos, 1, &types_after, state.schema()) {
            return None;
        }
        tr.split_into(pos, 1, &types_after).ok()?;
    } else if pine_richtext::transform::can_split(tr.doc(), pos, 1, state.schema()) {
        tr.split(pos, 1).ok()?;
    } else {
        let types_after = paragraph_after();
        if !can_split_into(tr.doc(), pos, 1, &types_after, state.schema()) {
            return None;
        }
        tr.split_into(pos, 1, &types_after).ok()?;
    }
    let selection = Selection::near(tr.doc(), state.schema(), pos + 2, 1).ok()?;
    tr.set_selection(selection).ok()?;
    Some(tr)
}

fn run_case<R, F>(name: &str, mut f: F)
where
    F: FnMut() -> R,
{
    let started = Instant::now();
    black_box(f());
    let warm = started.elapsed();
    let iterations = iterations_for(warm);

    let started = Instant::now();
    for _ in 0..iterations {
        black_box(f());
    }
    let total = started.elapsed();
    let per_op = total.as_secs_f64() * 1_000.0 / iterations as f64;

    println!(
        "{name:<58} {iterations:>6} iters {total_ms:>10.3} ms total {per_op:>10.4} ms/op (warm {warm_ms:.4} ms)",
        total_ms = total.as_secs_f64() * 1_000.0,
        warm_ms = warm.as_secs_f64() * 1_000.0,
    );
}

fn iterations_for(warm: Duration) -> u64 {
    if warm >= Duration::from_millis(100) {
        return 1;
    }
    let warm_nanos = warm.as_nanos().max(1);
    let target_nanos = Duration::from_millis(250).as_nanos();
    let estimated = target_nanos / warm_nanos;
    estimated.clamp(100, 20_000) as u64
}

fn bench_state(label: &str, state: &EditorState) {
    let schema = state.schema().clone();
    let doc = state.doc().clone();
    let pos = state.selection().from(state.doc());
    let enter = chain_commands(vec![
        lift_empty_block(),
        split_list_item(&["list_item", "task_item"]),
        split_block(),
    ]);
    let split = split_block();

    println!("\n== {label} ==");
    run_case("enter_chain/current", || {
        let tr = enter.apply(state).expect("enter should apply");
        tr.transform().steps().len()
    });
    run_case("split_block/current", || {
        let tr = split.apply(state).expect("split block should apply");
        tr.transform().steps().len()
    });
    run_case("split_block/schema_scan_match_type_after", || {
        let tr = legacy_split_block_apply(state).expect("legacy split should apply");
        tr.transform().steps().len()
    });
    run_case("default_after/schema_scan_match_type", || {
        legacy_default_textblock_after_match_type(&doc, pos, &schema)
    });
    run_case("default_after/content_match_default_textblock", || {
        current_default_textblock_after_can_replace(&doc, pos, &schema)
    });
    run_case("candidate/title_match_type", || {
        match_type_candidate(&doc, pos, "title", &schema)
    });
    run_case("candidate/title_can_replace_with", || {
        can_replace_candidate(&doc, pos, "title", &schema)
    });
    run_case("candidate/paragraph_match_type", || {
        match_type_candidate(&doc, pos, "paragraph", &schema)
    });
    run_case("candidate/paragraph_can_replace_with", || {
        can_replace_candidate(&doc, pos, "paragraph", &schema)
    });
    run_case("can_split_into/paragraph_after", || {
        let types_after = paragraph_after();
        can_split_into(&doc, pos, 1, &types_after, &schema)
    });
    run_case("transform/split_into_paragraph_after", || {
        let types_after = paragraph_after();
        let mut tr = Transform::new(schema.clone(), doc.clone());
        tr.split_into(pos, 1, &types_after).unwrap();
        tr.doc().child_count()
    });
    run_case("selection/near_after_split", || {
        let types_after = paragraph_after();
        let mut tr = Transform::new(schema.clone(), doc.clone());
        tr.split_into(pos, 1, &types_after).unwrap();
        Selection::near(tr.doc(), &schema, pos + 2, 1).unwrap()
    });
}

fn bench_delete_state(label: &str, state: &EditorState) {
    let schema = state.schema().clone();
    let doc = state.doc().clone();
    let pos = state.selection().from(state.doc());
    let backspace = chain_commands(vec![
        delete_selection(),
        join_backward(),
        select_node_backward(),
    ]);
    let delete = chain_commands(vec![
        delete_selection(),
        join_forward(),
        select_node_forward(),
    ]);

    println!("\n== {label} ==");
    run_case("delete/backward_char_transaction", || {
        let mut tr = state.tr();
        tr.delete(pos - 1, pos).unwrap();
        tr.transform().steps().len()
    });
    run_case("delete/forward_char_transaction", || {
        let mut tr = state.tr();
        tr.delete(pos, pos + 1).unwrap();
        tr.transform().steps().len()
    });
    run_case("keydown/backspace_command_chain_noop_inside_text", || {
        backspace
            .apply(state)
            .map(|tr| tr.transform().steps().len())
    });
    run_case("keydown/delete_command_chain_noop_inside_text", || {
        delete.apply(state).map(|tr| tr.transform().steps().len())
    });
    run_case("transform/delete_backward_char", || {
        let mut tr = Transform::new(schema.clone(), doc.clone());
        tr.delete(pos - 1, pos).unwrap();
        tr.doc().content_size()
    });
    run_case("transform/delete_forward_char", || {
        let mut tr = Transform::new(schema.clone(), doc.clone());
        tr.delete(pos, pos + 1).unwrap();
        tr.doc().content_size()
    });
}

fn main() {
    println!("pine-richtext command hot-path benchmark");
    println!("schema: Keep title + default block/list/task extensions");
    bench_state("Keep title end -> paragraph", &keep_title_only_state());
    bench_state("Keep H1 end -> paragraph", &keep_heading_state());
    bench_delete_state(
        "Keep paragraph middle repeated delete",
        &keep_paragraph_state(
            "abcdefghijklmnopqrstuvwxyz abcdefghijklmnopqrstuvwxyz abcdefghijklmnopqrstuvwxyz",
            42,
        ),
    );
}
