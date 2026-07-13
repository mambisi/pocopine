//! Markdown (CommonMark) serializer for rich-text documents.
//!
//! Walks a [`crate::model::Node`] tree, emits a stream of
//! [`pulldown_cmark::Event`]s in the order the upstream CommonMark
//! parser would have produced for the equivalent markdown source,
//! and renders the events via
//! [`pulldown_cmark_to_cmark::cmark`].
//!
//! Why not a 1:1 port of `prosemirror-markdown/src/to_markdown.ts`?
//! That serializer carries a decade of regex-based escape rules
//! and mark-mixing heuristics that compensate for JS having no
//! standard CommonMark AST. Rust's
//! [`pulldown-cmark`](https://docs.rs/pulldown-cmark) +
//! [`pulldown-cmark-to-cmark`](https://docs.rs/pulldown-cmark-to-cmark)
//! pair handles the same surface (escape rules, list nesting,
//! mark reordering, autolink detection) correctly out of the box —
//! using them means correctness lives in libraries with millions
//! of round-tripped documents behind them instead of our
//! hand-rolled regex.
//!
//! This module ships:
//!
//! * [`MarkdownSerializer`] — config object with an opt-in
//!   per-node-type override table (apps with custom node types
//!   register an emitter that pushes their own events).
//! * [`serialize`] — convenience function: walk a doc, render to
//!   `String`.
//! * [`NodeEmitter`] — the per-type emitter callback type apps
//!   register via [`MarkdownSerializer::register_node`].
//! * [`EventSink`] — handle passed into [`NodeEmitter`] callbacks
//!   for pushing events / recursing into children.
//!
//! For now this covers the schema_basic vocabulary: `doc`,
//! `paragraph`, `heading`, `blockquote`, `code_block`,
//! `horizontal_rule`, `bullet_list`, `ordered_list`, `list_item`,
//! `image`, `hard_break`, `text`, plus the marks `em`, `strong`,
//! `link`, `code`. Apps with extra node types (e.g. task lists)
//! register an override; unknown node types are emitted with their
//! children inlined and a `tracing::warn!` so authors notice.

use std::collections::HashMap;
use std::sync::Arc;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, LinkType, Tag, TagEnd};
use pulldown_cmark_to_cmark::{
    Options as RenderOptions, calculate_code_block_token_count, cmark_with_options,
};

/// Re-export `pulldown_cmark` so extension authors can build
/// [`MarkdownParseRule`] `get_attrs` closures and [`NodeEmitter`]
/// callbacks without taking pulldown-cmark as a direct
/// dependency. Use `pine_richtext::markdown::pulldown_cmark::{Event, Tag, …}`.
pub use ::pulldown_cmark;

/// Default fence length when no code block is present (matches the
/// library's `DEFAULT_OPTIONS.code_block_token_count = 4`).
const DEFAULT_CODE_BLOCK_TOKEN_COUNT: usize = 4;

use crate::model::{Mark, Node};

/// Per-node-type event emitter. Receives the node, the parent (or
/// the same node if it's the root), the child index in the parent
/// (0 for the root), and an [`EventSink`] to push events into.
///
/// Apps with custom node types register one of these to interpose
/// on the default walker — e.g. a task-list extension might emit
/// `[ ] ` / `[x] ` text prefixes inside each `list_item`.
pub type NodeEmitter = Arc<dyn Fn(&Node, &Node, usize, &mut EventSink<'_>) + Send + Sync + 'static>;

/// Per-mark-type event emitter. Returns how the mark should render
/// around its text run: either as a [`Tag`] pair the inner content
/// is wrapped in (em / strong / link / strikethrough), or as an
/// atomic [`Event`] that REPLACES the text run entirely (inline
/// `code` is the canonical case — `Event::Code(text)` swallows the
/// run and other marks are dropped).
pub type MarkEmitter = Arc<dyn Fn(&Mark) -> MarkRender + Send + Sync + 'static>;

/// Output of a [`MarkEmitter`].
///
/// Mirrors the two-way split CommonMark makes for inline marks:
/// most marks open a Tag and close it after the inner content;
/// inline code spans are atomic single events. Extensions that
/// add novel marks (highlight, underline-as-html, …) pick the
/// variant that matches their intended markdown shape.
pub enum MarkRender {
    /// Wrap the inner text run in `Start(tag) … End(tag.to_end())`.
    /// The text inside is escaped and may carry other (non-atomic)
    /// marks layered inside this one.
    Wrap(Tag<'static>),
    /// Replace the text run with a single atomic [`Event`]. Other
    /// marks on the run are dropped (the atomic event swallows
    /// them — CommonMark inline code can't nest emphasis).
    Atomic(Arc<dyn Fn(&str) -> Event<'static> + Send + Sync + 'static>),
}

/// Declarative parse rule contributed by an extension.
///
/// Each rule claims a pulldown-cmark event ([`ParseMatch`]) and
/// describes what model construct it maps to ([`ParseMapping`]).
/// The walker checks the contributed rule table first and falls
/// back to its built-in handling for events no rule claims —
/// extensions can either override the default or just add new
/// shapes.
///
/// Mirrors the `tokens` table in `prosemirror-markdown`'s parser
/// and the `addStorage().markdown.parse` shape in TipTap, adapted
/// to pulldown-cmark's event vocabulary.
pub struct MarkdownParseRule {
    /// Which event the rule fires on.
    pub matches: ParseMatch,
    /// What model construct to build.
    pub maps_to: ParseMapping,
}

/// Which pulldown-cmark event a [`MarkdownParseRule`] claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParseMatch {
    /// `Event::Start(Tag::*)` and its matching `Event::End(TagEnd::*)`
    /// pair. The walker fires the rule's mapping on the open and
    /// closes it automatically on the matching end.
    Tag(TagKind),
    /// An atomic `Event` variant (no Start/End pair).
    Event(EventKind),
}

/// Variant selector for `pulldown_cmark::Tag`. Names mirror the
/// upstream enum so cross-referencing is mechanical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagKind {
    Paragraph,
    Heading,
    BlockQuote,
    CodeBlock,
    HtmlBlock,
    List,
    Item,
    FootnoteDefinition,
    DefinitionList,
    DefinitionListTitle,
    DefinitionListDefinition,
    Table,
    TableHead,
    TableRow,
    TableCell,
    Emphasis,
    Strong,
    Strikethrough,
    Superscript,
    Subscript,
    Link,
    Image,
    MetadataBlock,
}

/// Variant selector for atomic `pulldown_cmark::Event`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    Code,
    HardBreak,
    SoftBreak,
    Rule,
    TaskListMarker,
    InlineMath,
    DisplayMath,
    FootnoteReference,
    Html,
    InlineHtml,
}

/// What model construct a [`MarkdownParseRule`] builds from its
/// claimed event(s).
pub enum ParseMapping {
    /// Open a block-level container. The walker pushes a custom
    /// block builder; child events accumulate as `Node` children
    /// until the matching End event. On close, the builder
    /// constructs `schema.node(node_type, get_attrs(start), …)`
    /// and appends it to the parent.
    Block {
        node_type: String,
        get_attrs: Option<ParseAttrFn>,
    },
    /// Open a block whose concrete node type and attrs depend on the
    /// surrounding parse frames. This keeps contextual formats declarative:
    /// GFM `TableCell`, for example, resolves to `table_header_cell` while a
    /// `TableHead` frame is open and to `table_cell` in body rows.
    ContextualBlock(ParseContextualBlockFn),
    /// Push an inline mark onto the active stack for the duration
    /// of the Start/End scope. Text events inside carry the mark.
    Mark {
        mark_type: String,
        get_attrs: Option<ParseAttrFn>,
    },
    /// Emit an atomic leaf node (no children, no scope). For
    /// `Event` rules and `Tag` rules that have empty content
    /// (e.g. horizontal rules, hard breaks).
    LeafNode {
        node_type: String,
        get_attrs: Option<ParseAttrFn>,
    },
    /// Escape hatch for shapes the declarative variants don't
    /// cover. The callback receives the triggering event and a
    /// mutable [`ParseSink`] giving direct access to the walker's
    /// block stack and the schema. Used by
    /// `TaskListExtension`'s rule which mutates the parent Item's
    /// `checked` attr instead of producing a node.
    Custom(ParseCustomFn),
}

/// Closure type alias for [`ParseMapping::Custom`] callbacks.
/// Factored out so the trait-object type doesn't trip Clippy's
/// `type_complexity` lint at every use site.
pub type ParseCustomFn =
    Arc<dyn Fn(&Event<'_>, &mut ParseSink<'_>) -> Result<(), ParseError> + Send + Sync>;

/// Resolver for [`ParseMapping::ContextualBlock`].
///
/// The returned pair is `(node_type, attrs)`. The context is read-only and
/// exposes only stable facts about enclosing extension block frames; it cannot
/// mutate parser state or append nodes as a side effect.
pub type ParseContextualBlockFn =
    Arc<dyn Fn(&Event<'_>, &ParseContext<'_>) -> (String, Attrs) + Send + Sync>;

/// Closure shape for `MarkdownParseRule::get_attrs`. The event
/// passed in is the Start event for `Tag` rules or the atomic
/// event for `Event` rules.
pub type ParseAttrFn = Arc<dyn Fn(&Event<'_>) -> Attrs + Send + Sync + 'static>;

/// Read-only context for resolving an extension block from its ancestors.
pub struct ParseContext<'a> {
    block_stack: &'a [Block],
}

impl ParseContext<'_> {
    /// Whether a contributed ancestor block was opened by `tag`.
    pub fn has_enclosing_tag(&self, tag: TagKind) -> bool {
        self.block_stack.iter().rev().any(|block| {
            matches!(
                block,
                Block::Custom {
                    source_tag: Some(source),
                    ..
                } if *source == tag
            )
        })
    }

    /// Number of already-finished children in the nearest contributed block
    /// named `node_type`.
    pub fn child_count(&self, node_type: &str) -> Option<usize> {
        self.block_stack.iter().rev().find_map(|block| match block {
            Block::Custom {
                node_type: candidate,
                children,
                ..
            } if candidate == node_type => Some(children.len()),
            _ => None,
        })
    }

    /// Opening event for the nearest contributed block named `node_type`.
    ///
    /// This lets descendants read source metadata without smuggling it into
    /// the durable node attrs. GFM table column alignments are the canonical
    /// use: they live on `Start(Tag::Table)` and are projected onto each cell.
    pub fn opening_event(&self, node_type: &str) -> Option<&Event<'static>> {
        self.block_stack.iter().rev().find_map(|block| match block {
            Block::Custom {
                node_type: candidate,
                source_event,
                ..
            } if candidate == node_type => Some(source_event),
            _ => None,
        })
    }
}

/// Mutable handle passed to [`ParseMapping::Custom`] callbacks.
/// Exposes a minimal API for appending model nodes to the
/// current container — sufficient for atomic-event rules that
/// produce a single Node (or no Node, e.g. TaskListMarker which
/// just mutates the enclosing Item's attrs).
///
/// Custom callbacks needing to inspect or mutate deeper walker
/// state (e.g. the block-builder stack) are out of scope; the
/// shape above covers GFM task lists, footnote-style refs, and
/// other simple shapes without exposing internal state. Heavier
/// extensions can register `Block` / `Mark` / `LeafNode`
/// mappings declaratively and avoid Custom entirely.
pub struct ParseSink<'a> {
    /// Walker schema for building nodes.
    schema: &'a Schema,
    /// Mutable borrow of the walker's block stack — callbacks
    /// reach into the current builder via the dedicated
    /// helper methods below rather than the raw field.
    block_stack: &'a mut Vec<Block>,
}

impl<'a> ParseSink<'a> {
    /// Schema accessor — Custom callbacks can construct nodes
    /// via the same schema the walker uses.
    pub fn schema(&self) -> &Schema {
        self.schema
    }

    /// Set an attribute on the closest enclosing `list_item`-
    /// shape builder (the current Item). Returns `true` if an
    /// Item builder was found and updated; `false` if the
    /// callback fired outside any list item. Used by the
    /// built-in GFM task-list marker rule.
    pub fn set_current_item_attr(&mut self, key: &str, value: serde_json::Value) -> bool {
        for block in self.block_stack.iter_mut().rev() {
            if let Block::Item { attrs, .. } = block {
                attrs.insert(key.into(), value);
                return true;
            }
        }
        false
    }

    /// Flag the closest enclosing `List` builder as a task list.
    /// On close the list emits as `task_list`/`task_item` (or
    /// falls back to `bullet_list` if the schema doesn't declare
    /// the task types).
    pub fn flag_enclosing_list_as_task(&mut self) -> bool {
        for block in self.block_stack.iter_mut().rev() {
            if let Block::List { is_task, .. } = block {
                *is_task = true;
                return true;
            }
        }
        false
    }
}

/// Mutable handle passed to [`NodeEmitter`] closures. Lets a
/// custom emitter both push events and recurse into children using
/// the default walker (so a wrapper-style emitter only needs to
/// emit its frame and call `sink.render_content(node)`).
pub struct EventSink<'a> {
    serializer: &'a MarkdownSerializer,
    events: &'a mut Vec<Event<'static>>,
}

impl<'a> EventSink<'a> {
    /// Push an event into the output stream.
    pub fn push(&mut self, event: Event<'static>) {
        self.events.push(event);
    }

    /// Recurse into every child of `parent` with the default
    /// walker. Useful for wrapper-style emitters that want to add
    /// a frame around the content but reuse the default behavior
    /// for descendants.
    pub fn render_content(&mut self, parent: &Node) {
        for (i, child) in parent.content().iter().enumerate() {
            self.render_node(child, parent, i);
        }
    }

    /// Recurse into a single node, dispatching through the
    /// override table.
    pub fn render_node(&mut self, node: &Node, parent: &Node, index: usize) {
        if let Some(emitter) = self.serializer.nodes.get(node.type_name()) {
            emitter(node, parent, index, self);
        } else {
            self.serializer.default_emit(node, parent, index, self);
        }
    }
}

/// Markdown serializer.
///
/// Holds an opt-in per-node-type override table and an opt-in
/// per-mark-type override table — both empty by default. The
/// walker covers the schema_basic vocabulary out of the box, so
/// most schemas need no overrides at all. Construct with
/// [`MarkdownSerializer::new`] or pull a runtime-pre-folded
/// instance via [`crate::runtime::EditorRuntime::markdown_serializer`].
#[derive(Clone, Default)]
pub struct MarkdownSerializer {
    /// Override table — node type name → custom emitter. Unknown
    /// types fall through to the built-in default walker (which
    /// covers the schema_basic vocabulary).
    pub nodes: HashMap<String, NodeEmitter>,
    /// Override table — mark type name → custom emitter. Looked
    /// up first; if no entry exists, falls back to the built-in
    /// handling for `em` / `strong` / `link` / `code` and emits
    /// a warning for unknown mark types. Extension-contributed
    /// marks (strikethrough from a GFM extension, highlight from
    /// a custom extension, etc.) register here.
    pub marks: HashMap<String, MarkEmitter>,
}

impl MarkdownSerializer {
    /// Construct a serializer with no overrides (the schema_basic
    /// vocabulary is built into the walker, so most schemas need
    /// no overrides at all).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an emitter for a specific node type. Overrides any
    /// previous entry for the same type.
    pub fn register_node(mut self, type_name: impl Into<String>, emitter: NodeEmitter) -> Self {
        self.nodes.insert(type_name.into(), emitter);
        self
    }

    /// Register an emitter for a specific mark type. Overrides any
    /// previous entry. Use `MarkRender::Wrap(Tag::*)` for
    /// emphasis-style marks and `MarkRender::Atomic(...)` for
    /// inline-code-style marks that swallow the text run.
    pub fn register_mark(mut self, type_name: impl Into<String>, emitter: MarkEmitter) -> Self {
        self.marks.insert(type_name.into(), emitter);
        self
    }

    /// Walk `doc` and render to a `String`. `doc` is expected to
    /// be a top-level node whose children are block-level (the
    /// `doc` node in schema_basic).
    pub fn serialize(&self, doc: &Node) -> Result<String, SerializeError> {
        let mut events: Vec<Event<'static>> = Vec::new();
        {
            let mut sink = EventSink {
                serializer: self,
                events: &mut events,
            };
            // `doc` itself doesn't emit a tag; render its children
            // as top-level blocks.
            sink.render_content(doc);
        }
        let mut out = String::new();
        // Override `increment_ordered_list_bullets=true` — the
        // library's default is `false` (round-trip preservation
        // of source-style `1. ... 1. ...` lists), but we're
        // generating fresh markdown and want canonical
        // `1. ... 2. ... 3. ...` numbering.
        //
        // Compute `code_block_token_count` from the event stream
        // so a code block containing N consecutive backticks
        // emits an (N+1)-backtick fence instead of the fixed
        // default. Without this a `` ```` ``-containing block
        // would close prematurely.
        let code_block_token_count = calculate_code_block_token_count(events.iter())
            .unwrap_or(DEFAULT_CODE_BLOCK_TOKEN_COUNT);
        let options = RenderOptions {
            increment_ordered_list_bullets: true,
            code_block_token_count,
            ..RenderOptions::default()
        };
        cmark_with_options(events.iter(), &mut out, options).map_err(SerializeError::Format)?;
        Ok(out)
    }

    /// Default emit for a node type the override table doesn't
    /// handle. Covers the schema_basic vocabulary; unknown types
    /// inline their children with a `tracing::warn!`.
    fn default_emit(&self, node: &Node, parent: &Node, index: usize, sink: &mut EventSink<'_>) {
        match node.type_name() {
            "doc" => sink.render_content(node),
            "paragraph" => {
                sink.push(Event::Start(Tag::Paragraph));
                emit_inline_content(node, sink);
                sink.push(Event::End(TagEnd::Paragraph));
            }
            "heading" => {
                let level = node
                    .attrs()
                    .get("level")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.clamp(1, 6) as usize)
                    .unwrap_or(1);
                let level = HeadingLevel::try_from(level).unwrap_or(HeadingLevel::H1);
                sink.push(Event::Start(Tag::Heading {
                    level,
                    id: None,
                    classes: Vec::new(),
                    attrs: Vec::new(),
                }));
                emit_inline_content(node, sink);
                sink.push(Event::End(TagEnd::Heading(level)));
            }
            "blockquote" => {
                sink.push(Event::Start(Tag::BlockQuote(None)));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::BlockQuote(None)));
            }
            "code_block" => {
                let lang = node
                    .attrs()
                    .get("params")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                sink.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(
                    lang.to_string().into(),
                ))));
                let mut text = String::new();
                gather_text(node, &mut text);
                sink.push(Event::Text(text.into()));
                sink.push(Event::End(TagEnd::CodeBlock));
            }
            "horizontal_rule" => {
                sink.push(Event::Rule);
            }
            "bullet_list" => {
                sink.push(Event::Start(Tag::List(None)));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::List(false)));
            }
            "ordered_list" => {
                let order = node
                    .attrs()
                    .get("order")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                sink.push(Event::Start(Tag::List(Some(order))));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::List(true)));
            }
            "list_item" => {
                sink.push(Event::Start(Tag::Item));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::Item));
            }
            "image" => {
                let src = node
                    .attrs()
                    .get("src")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let alt = node
                    .attrs()
                    .get("alt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = node
                    .attrs()
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                sink.push(Event::Start(Tag::Image {
                    link_type: LinkType::Inline,
                    dest_url: src.into(),
                    title: title.into(),
                    id: "".into(),
                }));
                if !alt.is_empty() {
                    // Apply the same first-char-aware escape we
                    // use for inline text — `alt="a]b"` would
                    // otherwise serialize as `![a]b](url)` and
                    // truncate the alt at the unescaped `]`.
                    sink.push(Event::Text(escape_inline_text(&alt).into()));
                }
                sink.push(Event::End(TagEnd::Image));
            }
            "hard_break" => {
                sink.push(Event::HardBreak);
            }
            "text" => emit_text(node, parent, index, sink),
            other => {
                tracing::warn!(
                    target: "pocopine.log",
                    node_type = other,
                    "MarkdownSerializer: no emitter for node type; \
                     inlining children. Register a NodeEmitter for this \
                     type via `MarkdownSerializer::register_node`."
                );
                sink.render_content(node);
            }
        }
    }
}

/// Walk a node's content as inline content — emit Text events
/// with mark frames around them.
fn emit_inline_content(parent: &Node, sink: &mut EventSink<'_>) {
    for (i, child) in parent.content().iter().enumerate() {
        sink.render_node(child, parent, i);
    }
}

/// Emit a text node with its marks. Marks are opened in the order
/// they appear on the node, closed in reverse — matches the
/// canonical Tag::Start / Tag::End pairing that
/// pulldown-cmark-to-cmark expects.
fn emit_text(node: &Node, parent: &Node, _index: usize, sink: &mut EventSink<'_>) {
    let text = node.text().unwrap_or("");
    if text.is_empty() {
        return;
    }

    // Resolve each mark to a `MarkRender`. We consult the
    // serializer's `marks` table first (extension-contributed)
    // then fall back to the built-in handling for the four
    // schema_basic marks.
    let renders: Vec<MarkRender> = node
        .marks()
        .iter()
        .filter_map(|m| resolve_mark_render(sink.serializer, m, parent))
        .collect();

    // Atomic marks (canonical example: inline `code`) replace the
    // text run with a single event and drop all other marks for
    // the run — CommonMark inline code can't nest emphasis. If
    // any active mark is atomic, the FIRST one wins.
    if let Some(atomic) = renders.iter().find_map(|r| match r {
        MarkRender::Atomic(f) => Some(f.clone()),
        _ => None,
    }) {
        sink.push(atomic(text));
        return;
    }

    // All Wrap marks — open in order, emit escaped text, close in reverse.
    let wraps: Vec<Tag<'static>> = renders
        .into_iter()
        .filter_map(|r| match r {
            MarkRender::Wrap(tag) => Some(tag),
            MarkRender::Atomic(_) => None,
        })
        .collect();

    for tag in &wraps {
        sink.push(Event::Start(tag.clone()));
    }
    sink.push(Event::Text(escape_inline_text(text).into()));
    for tag in wraps.into_iter().rev() {
        sink.push(Event::End(tag.to_end()));
    }
}

/// Look up the [`MarkRender`] for `mark`, preferring the
/// serializer's contributed table. Falls back to the built-in
/// handling for `em` / `strong` / `link` / `code` so schemas that
/// don't register marks via extensions still get the standard
/// CommonMark output.
fn resolve_mark_render(
    serializer: &MarkdownSerializer,
    mark: &Mark,
    _parent: &Node,
) -> Option<MarkRender> {
    if let Some(emitter) = serializer.marks.get(mark.type_name()) {
        return Some(emitter(mark));
    }
    builtin_mark_render(mark)
}

/// Built-in fallback for the schema_basic mark vocabulary.
/// Returns `None` for unknown mark types (logged once).
fn builtin_mark_render(mark: &Mark) -> Option<MarkRender> {
    match mark.type_name() {
        "em" => Some(MarkRender::Wrap(Tag::Emphasis)),
        "strong" => Some(MarkRender::Wrap(Tag::Strong)),
        "link" => {
            let href = mark
                .attrs()
                .get("href")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = mark
                .attrs()
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(MarkRender::Wrap(Tag::Link {
                link_type: LinkType::Inline,
                dest_url: href.into(),
                title: title.into(),
                id: "".into(),
            }))
        }
        "code" => Some(MarkRender::Atomic(Arc::new(|text: &str| {
            Event::Code(text.to_string().into())
        }))),
        other => {
            tracing::warn!(
                target: "pocopine.log",
                mark_type = other,
                "MarkdownSerializer: no emitter for mark type; dropped"
            );
            None
        }
    }
}

/// Backslash-escape the CommonMark inline-special set so that
/// raw model text (`a*b`, `[anchor]`, `` `code` ``) doesn't get
/// re-parsed as inline markup on a round-trip. The set covers
/// `\` `` ` `` `*` `_` `[` `]` `<` — enough to neutralize
/// emphasis, inline code, link syntax, and inline-HTML /
/// autolink starts. We deliberately don't escape `>` `#` `+`
/// `-` `.` `!` mid-line: `>` only opens a blockquote at start
/// of line (handled by the renderer's first-char pass) and is
/// harmless mid-line once `<` is neutralized.
///
/// We don't reuse pulldown-cmark-to-cmark's
/// `escape_special_characters` because that helper only escapes
/// the first / last character of a text run — it assumes the
/// inputs came from already-parsed markdown where mid-line
/// specials are already escaped. We're going the other way
/// (model → markdown) so we have to neutralize them ourselves.
///
/// **First-character handoff:** if the first char would itself
/// be escaped by the renderer's first-char pass (the BASE
/// special set `#\_*<>` + `` ` `` + `|[]`), we DON'T pre-escape
/// it here — otherwise the renderer adds a second backslash and
/// the round-trip produces a literal `\` artifact (e.g. `*todo`
/// → my pass `\*todo` → renderer `\\*todo` → reparse `\*todo`).
fn escape_inline_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &ch) in chars.iter().enumerate() {
        let first = i == 0;
        let needs_escape = matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<');
        let renderer_will_first_char_escape = first && is_renderer_first_char_special(ch);
        let intra_word_underscore = ch == '_'
            && i > 0
            && i + 1 < chars.len()
            && is_word_char(chars[i - 1])
            && is_word_char(chars[i + 1]);
        if needs_escape && !renderer_will_first_char_escape && !intra_word_underscore {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// CommonMark "word character" for the intra-word underscore
/// exception: ASCII letter, digit, or underscore. `foo_bar` has
/// `_` between two word chars → not emphasis, no escape needed.
fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// The character set
/// [`pulldown_cmark_to_cmark::Options::special_characters`]
/// reports under default options. The renderer auto-escapes the
/// FIRST character of each Text event if it falls in this set,
/// so we must not pre-escape those chars here.
fn is_renderer_first_char_special(ch: char) -> bool {
    matches!(
        ch,
        '#' | '\\' | '_' | '*' | '<' | '>' | '`' | '|' | '[' | ']'
    )
}

/// Recursively gather all descendant text into `buf`. Used by
/// code_block (which is opaque to inline rendering — the body is
/// emitted as a single Text event).
fn gather_text(node: &Node, buf: &mut String) {
    if let Some(text) = node.text() {
        buf.push_str(text);
        return;
    }
    for child in node.content().iter() {
        gather_text(child, buf);
    }
}

/// Errors from [`MarkdownSerializer::serialize`].
#[derive(Debug)]
pub enum SerializeError {
    /// `pulldown-cmark-to-cmark` rejected the event stream (should
    /// only happen if a `NodeEmitter` produces an unbalanced or
    /// malformed sequence).
    Format(pulldown_cmark_to_cmark::Error),
}

impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(e) => write!(f, "markdown formatter rejected event stream: {e}"),
        }
    }
}

impl std::error::Error for SerializeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(e) => Some(e),
        }
    }
}

/// Convenience: serialize a doc with the default node table (no
/// overrides). Equivalent to `MarkdownSerializer::new().serialize(doc)`.
pub fn serialize(doc: &Node) -> Result<String, SerializeError> {
    MarkdownSerializer::new().serialize(doc)
}

// =========================================================================
// Parser — markdown text → Node tree
// =========================================================================

use crate::model::{Attrs, Fragment, Schema};
use pulldown_cmark::{Options as ParserOptions, Parser};
use serde_json::json;

/// Errors from [`MarkdownParser::parse`].
#[derive(Debug)]
pub enum ParseError {
    /// Schema rejected a node we tried to construct — usually because
    /// the schema doesn't declare the type (e.g. parsing a heading
    /// against a paragraph-only schema). Contains the failing node
    /// type name and the underlying error.
    Schema { node_type: String, error: String },
    /// The parser produced no top-level content. Shouldn't normally
    /// happen — even an empty markdown string produces an empty doc
    /// with one paragraph — but reported defensively.
    Empty,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema { node_type, error } => {
                write!(f, "schema rejected `{node_type}`: {error}")
            }
            Self::Empty => write!(f, "parser produced no content"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Markdown parser — walks `pulldown_cmark::Event`s, builds a
/// [`Node`] tree validated against the provided schema.
///
/// Configuration: parser options (which GFM features are enabled)
/// and an opt-in rule table keyed by [`ParseMatch`]. Apps with
/// custom shapes register [`MarkdownParseRule`]s either directly
/// via [`MarkdownParser::register_rule`] or — preferred — via
/// the extension contract
/// ([`crate::extension::RichTextExtension::markdown_parse_rules`])
/// so the runtime folds them into a per-instance parser.
#[derive(Clone)]
pub struct MarkdownParser {
    /// `pulldown_cmark::Options` — defaults to `ENABLE_TASKLISTS`
    /// + `ENABLE_STRIKETHROUGH` so the parser recognizes the same
    ///   shapes the serializer emits (TaskListMarker events feed the
    ///   `task_list` / `task_item` builder; strikethrough survives a
    ///   round trip).
    pub options: ParserOptions,
    /// Extension-contributed parse rules. The walker checks this
    /// table BEFORE its built-in handling for each event, so
    /// extensions can either add new shapes (tables, callouts) or
    /// override defaults (e.g. a custom paragraph type). Stored
    /// as `Arc<MarkdownParseRule>` so multiple parsers built
    /// from the same runtime share the table cheaply.
    pub rules: HashMap<ParseMatch, Arc<MarkdownParseRule>>,
}

impl Default for MarkdownParser {
    fn default() -> Self {
        let mut options = ParserOptions::empty();
        options.insert(ParserOptions::ENABLE_TASKLISTS);
        options.insert(ParserOptions::ENABLE_STRIKETHROUGH);
        // Deliberately NOT enabling `ENABLE_TABLES` by default:
        // without a registered `Tag::Table*` parse rule, the
        // walker drops table tags and treats inner cell text as
        // loose paragraphs — a `| A | B |\n| 1 | 2 |` source
        // would import as four separate paragraphs `A`/`B`/`1`/`2`.
        // The runtime-aware constructor in
        // `EditorRuntime::markdown_parser` enables tables ONLY
        // when a Table parse rule is registered; apps wanting
        // tables without a runtime can `parser.options |=
        // Options::ENABLE_TABLES` themselves.
        Self {
            options,
            rules: HashMap::new(),
        }
    }
}

impl MarkdownParser {
    /// Construct a parser with default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a parse rule. The rule fires whenever the walker
    /// encounters an event matching its [`ParseMatch`] — taking
    /// precedence over the built-in handling for the same event.
    /// Returns `self` for fluent chaining.
    pub fn register_rule(mut self, rule: MarkdownParseRule) -> Self {
        self.rules.insert(rule.matches, Arc::new(rule));
        self
    }

    /// Parse `md` against `schema` and return a `doc` Node whose
    /// children are the top-level blocks.
    ///
    /// Empty input produces a doc with a single empty paragraph
    /// (CommonMark's notion of a valid minimal document). Block
    /// shapes the schema doesn't accept (e.g. heading against a
    /// paragraph-only schema) produce a [`ParseError::Schema`].
    pub fn parse(&self, md: &str, schema: &Schema) -> Result<Node, ParseError> {
        let parser = Parser::new_ext(md, self.options);
        let mut walker = ParseWalker::new(schema, &self.rules);
        for event in parser {
            walker.handle(event)?;
        }
        walker.finish()
    }
}

/// Convenience: parse with default options + the supplied schema.
/// No extension rules — for runtime-aware parsing (with custom
/// shapes from `TaskListExtension` etc.) use
/// [`crate::runtime::EditorRuntime::markdown_parser`].
pub fn parse(md: &str, schema: &Schema) -> Result<Node, ParseError> {
    MarkdownParser::new().parse(md, schema)
}

/// Block-level builder in the parse stack. Each variant accumulates
/// the children produced between its `Start(Tag)` event and the
/// matching `End(TagEnd)`. On close, the builder's `into_node`
/// constructs a validated `Node` via the schema and the builder is
/// popped off the stack — its node appended to the parent builder's
/// children.
#[allow(clippy::enum_variant_names)]
enum Block {
    Doc {
        children: Vec<Node>,
    },
    Paragraph {
        children: Vec<Node>,
    },
    Heading {
        level: usize,
        children: Vec<Node>,
    },
    Blockquote {
        children: Vec<Node>,
    },
    CodeBlock {
        params: String,
        text: String,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Node>,
        /// Set to `true` when ANY child item carries a
        /// `TaskListMarker` event. On close the list type changes
        /// from `bullet_list`/`ordered_list` to `task_list`. The
        /// schema must declare `task_list`/`task_item` for this to
        /// succeed; absent that the parser falls back to bullet
        /// list (logged as a warning).
        is_task: bool,
    },
    Item {
        children: Vec<Node>,
        /// Attributes accumulated during the item's scope. The
        /// built-in walker writes `checked` here when a
        /// `TaskListMarker(bool)` event fires; the
        /// [`ParseMapping::Custom`] callback for the task-list
        /// rule uses [`ParseSink::set_current_item_attr`] to do
        /// the same thing for app-contributed shapes.
        attrs: Attrs,
    },
    /// Image content collector. Pulldown-cmark emits the alt text
    /// as `Text` events between `Start(Image)` and `End(Image)`;
    /// we collect it into a single string instead of building text
    /// nodes (images are leaves in our model).
    Image {
        src: String,
        title: String,
        alt: String,
    },
    /// Generic block frame opened by an extension-contributed
    /// [`MarkdownParseRule`] with a [`ParseMapping::Block`].
    /// Treated like Blockquote / Doc / Item for the purposes of
    /// child accumulation: nested blocks append as `Node`s,
    /// inline content auto-wraps in paragraphs. On close the
    /// frame produces `schema.node(node_type, attrs, ...)`.
    Custom {
        node_type: String,
        attrs: Attrs,
        children: Vec<Node>,
        /// Tag kind and opening event that created this contributed frame.
        /// Retained only during parsing for contextual child resolution.
        source_tag: Option<TagKind>,
        source_event: Event<'static>,
    },
}

struct ParseWalker<'a> {
    schema: &'a Schema,
    /// Extension-contributed rules. Consulted in
    /// `open_tag` / `close_tag` / `handle` before the built-in
    /// match arms — rules can override defaults or add new
    /// shapes (tables, callouts) without core changes.
    rules: &'a HashMap<ParseMatch, Arc<MarkdownParseRule>>,
    block_stack: Vec<Block>,
    /// Active marks for any text emitted at the current scope.
    /// Pushed by `Start(Tag::Emphasis | Strong | Link)`; popped by
    /// the matching `End(TagEnd::*)`.
    active_marks: Vec<Mark>,
    /// Tracks which `Tag` rules opened a `Block::Custom` frame
    /// vs which pushed a mark, so the matching `End` knows what
    /// to pop. Indexed by depth of the open-tag-rule's push.
    custom_block_stack: Vec<String>,
}

impl<'a> ParseWalker<'a> {
    fn new(schema: &'a Schema, rules: &'a HashMap<ParseMatch, Arc<MarkdownParseRule>>) -> Self {
        Self {
            schema,
            rules,
            block_stack: vec![Block::Doc {
                children: Vec::new(),
            }],
            active_marks: Vec::new(),
            custom_block_stack: Vec::new(),
        }
    }

    /// Classify a `Tag` to its [`TagKind`] discriminator for rule
    /// lookup.
    fn tag_kind(tag: &Tag<'_>) -> TagKind {
        match tag {
            Tag::Paragraph => TagKind::Paragraph,
            Tag::Heading { .. } => TagKind::Heading,
            Tag::BlockQuote(_) => TagKind::BlockQuote,
            Tag::CodeBlock(_) => TagKind::CodeBlock,
            Tag::HtmlBlock => TagKind::HtmlBlock,
            Tag::List(_) => TagKind::List,
            Tag::Item => TagKind::Item,
            Tag::FootnoteDefinition(_) => TagKind::FootnoteDefinition,
            Tag::DefinitionList => TagKind::DefinitionList,
            Tag::DefinitionListTitle => TagKind::DefinitionListTitle,
            Tag::DefinitionListDefinition => TagKind::DefinitionListDefinition,
            Tag::Table(_) => TagKind::Table,
            Tag::TableHead => TagKind::TableHead,
            Tag::TableRow => TagKind::TableRow,
            Tag::TableCell => TagKind::TableCell,
            Tag::Emphasis => TagKind::Emphasis,
            Tag::Strong => TagKind::Strong,
            Tag::Strikethrough => TagKind::Strikethrough,
            Tag::Superscript => TagKind::Superscript,
            Tag::Subscript => TagKind::Subscript,
            Tag::Link { .. } => TagKind::Link,
            Tag::Image { .. } => TagKind::Image,
            Tag::MetadataBlock(_) => TagKind::MetadataBlock,
        }
    }

    /// Classify a `TagEnd` to its [`TagKind`] discriminator.
    /// Same disciminator as the matching Start so rule lookup
    /// finds the same rule on both ends.
    fn tag_end_kind(end: &TagEnd) -> TagKind {
        match end {
            TagEnd::Paragraph => TagKind::Paragraph,
            TagEnd::Heading(_) => TagKind::Heading,
            TagEnd::BlockQuote(_) => TagKind::BlockQuote,
            TagEnd::CodeBlock => TagKind::CodeBlock,
            TagEnd::HtmlBlock => TagKind::HtmlBlock,
            TagEnd::List(_) => TagKind::List,
            TagEnd::Item => TagKind::Item,
            TagEnd::FootnoteDefinition => TagKind::FootnoteDefinition,
            TagEnd::DefinitionList => TagKind::DefinitionList,
            TagEnd::DefinitionListTitle => TagKind::DefinitionListTitle,
            TagEnd::DefinitionListDefinition => TagKind::DefinitionListDefinition,
            TagEnd::Table => TagKind::Table,
            TagEnd::TableHead => TagKind::TableHead,
            TagEnd::TableRow => TagKind::TableRow,
            TagEnd::TableCell => TagKind::TableCell,
            TagEnd::Emphasis => TagKind::Emphasis,
            TagEnd::Strong => TagKind::Strong,
            TagEnd::Strikethrough => TagKind::Strikethrough,
            TagEnd::Superscript => TagKind::Superscript,
            TagEnd::Subscript => TagKind::Subscript,
            TagEnd::Link => TagKind::Link,
            TagEnd::Image => TagKind::Image,
            TagEnd::MetadataBlock(_) => TagKind::MetadataBlock,
        }
    }

    fn handle(&mut self, event: Event<'_>) -> Result<(), ParseError> {
        // Extension-contributed rule for atomic events takes
        // precedence. Tag-level rules are handled inside
        // open_tag / close_tag.
        if let Some(kind) = atomic_event_kind(&event)
            && let Some(rule) = self.rules.get(&ParseMatch::Event(kind)).cloned()
        {
            let owned = event.clone().into_static();
            return self.apply_rule_open(&rule, &owned);
        }
        match event {
            Event::Start(tag) => self.open_tag(tag)?,
            Event::End(end) => self.close_tag(end)?,
            Event::Text(text) => self.append_text(&text)?,
            Event::Code(text) => {
                // Image alt collector: a code span inside an
                // image's alt text (`` ![a `b` c](url) ``)
                // should land as literal alt text, not a text
                // node — otherwise `append_inline`'s
                // Block::Image arm would drop the run silently.
                if let Some(Block::Image { alt, .. }) = self.block_stack.last_mut() {
                    alt.push_str(&text);
                    return Ok(());
                }
                // Inline code is an atomic event — emit as a text
                // node with the `code` mark. PM upstream's
                // serializer says code is non-mixable / outermost,
                // so we drop other active marks for this run.
                let node = self
                    .schema
                    .text(text.to_string(), vec![self.code_mark()])
                    .map_err(|e| ParseError::Schema {
                        node_type: "text".into(),
                        error: e.to_string(),
                    })?;
                self.append_inline(node)?;
            }
            Event::HardBreak => {
                let node = self
                    .schema
                    .node("hard_break", Attrs::new(), Fragment::empty())
                    .map_err(|e| ParseError::Schema {
                        node_type: "hard_break".into(),
                        error: e.to_string(),
                    })?;
                self.append_inline(node)?;
            }
            Event::SoftBreak => {
                // Soft breaks become a single space — matches how
                // most renderers treat them (CommonMark spec § 6.8).
                self.append_text(" ")?;
            }
            Event::Rule => {
                let node = self
                    .schema
                    .node("horizontal_rule", Attrs::new(), Fragment::empty())
                    .map_err(|e| ParseError::Schema {
                        node_type: "horizontal_rule".into(),
                        error: e.to_string(),
                    })?;
                self.append_block(node)?;
            }
            Event::TaskListMarker(checked) => {
                // Sets the `checked` attr on the current Item AND
                // flips the parent List's `is_task` flag. The
                // `task_item` / `task_list` translation happens on
                // close. Stays built-in (not routed through the
                // pluggable rule table) because it mutates the
                // walker's internal `is_task` flag — extensions
                // can replicate the pattern via
                // `ParseSink::set_current_item_attr` +
                // `flag_enclosing_list_as_task` for any future
                // marker-style event.
                if let Some(Block::Item { attrs, .. }) = self.block_stack.last_mut() {
                    attrs.insert("checked".into(), json!(checked));
                }
                for block in self.block_stack.iter_mut().rev() {
                    if let Block::List { is_task, .. } = block {
                        *is_task = true;
                        break;
                    }
                }
            }
            // Footnotes, HTML, math, metadata blocks — not in the
            // schema_basic vocabulary. Drop with a warning.
            Event::Html(html) | Event::InlineHtml(html) => {
                tracing::warn!(
                    target: "pocopine.log",
                    "MarkdownParser: dropping inline/block HTML (`{html}`) — not in schema"
                );
            }
            Event::FootnoteReference(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {
                tracing::warn!(
                    target: "pocopine.log",
                    "MarkdownParser: dropping unsupported event {event:?}"
                );
            }
        }
        Ok(())
    }

    fn open_tag(&mut self, tag: Tag<'_>) -> Result<(), ParseError> {
        // Extension-contributed rule for this tag takes precedence
        // over built-in handling. The Start event we synthesize
        // for `get_attrs` callbacks owns its data (the Tag is
        // cloned into 'static lifetime, then wrapped in Event).
        let kind = Self::tag_kind(&tag);
        if let Some(rule) = self.rules.get(&ParseMatch::Tag(kind)).cloned() {
            let owned = Event::Start(tag.clone().into_static());
            return self.apply_rule_open(&rule, &owned);
        }
        match tag {
            Tag::Paragraph => self.block_stack.push(Block::Paragraph {
                children: Vec::new(),
            }),
            Tag::Heading { level, .. } => self.block_stack.push(Block::Heading {
                level: level as usize,
                children: Vec::new(),
            }),
            Tag::BlockQuote(_) => self.block_stack.push(Block::Blockquote {
                children: Vec::new(),
            }),
            Tag::CodeBlock(kind) => {
                let params = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.block_stack.push(Block::CodeBlock {
                    params,
                    text: String::new(),
                });
            }
            Tag::List(start) => self.block_stack.push(Block::List {
                ordered: start.is_some(),
                start: start.unwrap_or(1),
                items: Vec::new(),
                is_task: false,
            }),
            Tag::Item => self.block_stack.push(Block::Item {
                children: Vec::new(),
                attrs: Attrs::new(),
            }),
            Tag::Emphasis => self.active_marks.push(Mark::new("em", Attrs::new())),
            Tag::Strong => self.active_marks.push(Mark::new("strong", Attrs::new())),
            Tag::Link {
                dest_url, title, ..
            } => {
                let mut attrs = Attrs::new();
                attrs.insert("href".into(), json!(dest_url.to_string()));
                if !title.is_empty() {
                    attrs.insert("title".into(), json!(title.to_string()));
                }
                self.active_marks.push(Mark::new("link", attrs));
            }
            Tag::Image {
                dest_url, title, ..
            } => self.block_stack.push(Block::Image {
                src: dest_url.to_string(),
                title: title.to_string(),
                alt: String::new(),
            }),
            // Tables / definition lists / footnotes / metadata —
            // not in schema_basic. The Start event is dropped; we
            // still need a matching End handler so the
            // `close_tag` arm tolerates the End without finding
            // an open builder.
            other => {
                tracing::warn!(
                    target: "pocopine.log",
                    "MarkdownParser: ignoring unsupported tag start {other:?}"
                );
            }
        }
        Ok(())
    }

    fn close_tag(&mut self, end: TagEnd) -> Result<(), ParseError> {
        let kind = Self::tag_end_kind(&end);
        if let Some(rule) = self.rules.get(&ParseMatch::Tag(kind)).cloned() {
            return self.apply_rule_close(&rule);
        }
        match end {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::Image => {
                let block = self.block_stack.pop().ok_or(ParseError::Empty)?;
                let node = self.finish_block(block)?;
                self.append_block(node)?;
            }
            TagEnd::Emphasis => self.pop_mark("em"),
            TagEnd::Strong => self.pop_mark("strong"),
            TagEnd::Link => self.pop_mark("link"),
            // Unknown End events for tags we silently skipped on
            // Start. No-op.
            _ => {}
        }
        Ok(())
    }

    /// Apply a [`MarkdownParseRule`] on a Start tag — push the
    /// appropriate builder/mark onto the walker's stack.
    fn apply_rule_open(
        &mut self,
        rule: &Arc<MarkdownParseRule>,
        start_event: &Event<'static>,
    ) -> Result<(), ParseError> {
        match &rule.maps_to {
            ParseMapping::Block {
                node_type,
                get_attrs,
            } => {
                let attrs = get_attrs
                    .as_ref()
                    .map(|f| f(start_event))
                    .unwrap_or_default();
                self.block_stack.push(Block::Custom {
                    node_type: node_type.clone(),
                    attrs,
                    children: Vec::new(),
                    source_tag: match rule.matches {
                        ParseMatch::Tag(tag) => Some(tag),
                        ParseMatch::Event(_) => None,
                    },
                    source_event: start_event.clone(),
                });
                self.custom_block_stack.push(node_type.clone());
            }
            ParseMapping::ContextualBlock(resolve) => {
                let context = ParseContext {
                    block_stack: &self.block_stack,
                };
                let (node_type, attrs) = resolve(start_event, &context);
                self.block_stack.push(Block::Custom {
                    node_type: node_type.clone(),
                    attrs,
                    children: Vec::new(),
                    source_tag: match rule.matches {
                        ParseMatch::Tag(tag) => Some(tag),
                        ParseMatch::Event(_) => None,
                    },
                    source_event: start_event.clone(),
                });
                self.custom_block_stack.push(node_type);
            }
            ParseMapping::Mark {
                mark_type,
                get_attrs,
            } => {
                let attrs = get_attrs
                    .as_ref()
                    .map(|f| f(start_event))
                    .unwrap_or_default();
                self.active_marks.push(Mark::new(mark_type, attrs));
            }
            ParseMapping::LeafNode {
                node_type,
                get_attrs,
            } => {
                let attrs = get_attrs
                    .as_ref()
                    .map(|f| f(start_event))
                    .unwrap_or_default();
                let node = self
                    .schema
                    .node(node_type, attrs, Fragment::empty())
                    .map_err(|e| ParseError::Schema {
                        node_type: node_type.clone(),
                        error: e.to_string(),
                    })?;
                self.append_block(node)?;
                // Tag-level LeafNode: there'll be a matching End
                // event the close handler must tolerate. Push a
                // marker frame so `apply_rule_close` knows the
                // close has no Block to pop.
                self.custom_block_stack.push(String::new());
            }
            ParseMapping::Custom(callback) => {
                let mut sink = ParseSink {
                    schema: self.schema,
                    block_stack: &mut self.block_stack,
                };
                callback(start_event, &mut sink)?;
            }
        }
        Ok(())
    }

    /// Apply a rule on an End tag. For Block mappings, pops the
    /// `Block::Custom` frame and appends the resulting Node. For
    /// Mark mappings, pops the mark from `active_marks`. For
    /// LeafNode mappings, drops the marker frame from
    /// `custom_block_stack`. For Custom, the close fires the
    /// callback again with a synthesized end event.
    fn apply_rule_close(&mut self, rule: &Arc<MarkdownParseRule>) -> Result<(), ParseError> {
        match &rule.maps_to {
            ParseMapping::Block { .. } | ParseMapping::ContextualBlock(_) => {
                self.custom_block_stack.pop();
                let block = self.block_stack.pop().ok_or(ParseError::Empty)?;
                let node = self.finish_block(block)?;
                self.append_block(node)?;
            }
            ParseMapping::Mark { mark_type, .. } => {
                self.pop_mark(mark_type);
            }
            ParseMapping::LeafNode { .. } => {
                self.custom_block_stack.pop();
            }
            ParseMapping::Custom(_) => {
                // Custom callbacks are atomic for `Event` rules.
                // For `Tag` rules, the close is intentionally a
                // no-op — the callback runs only on open and is
                // expected to fully handle its scope there.
            }
        }
        Ok(())
    }

    fn pop_mark(&mut self, name: &str) {
        if let Some(idx) = self
            .active_marks
            .iter()
            .rposition(|m| m.type_name() == name)
        {
            self.active_marks.remove(idx);
        }
    }

    fn append_text(&mut self, s: &str) -> Result<(), ParseError> {
        if s.is_empty() {
            return Ok(());
        }
        // Image alt collector: text events inside an Image
        // builder feed the `alt` string, not new text nodes.
        if let Some(Block::Image { alt, .. }) = self.block_stack.last_mut() {
            alt.push_str(s);
            return Ok(());
        }
        // Code-block collector: text inside a CodeBlock builder
        // feeds the literal `text`, not a `text` node with a
        // `code` mark.
        if let Some(Block::CodeBlock { text, .. }) = self.block_stack.last_mut() {
            text.push_str(s);
            return Ok(());
        }
        let node = self
            .schema
            .text(s.to_string(), self.active_marks.clone())
            .map_err(|e| ParseError::Schema {
                node_type: "text".into(),
                error: e.to_string(),
            })?;
        self.append_inline(node)
    }

    /// Append an inline node (text, hard_break, inline code) to
    /// the current block. If the current block isn't a textblock,
    /// auto-wrap in a Paragraph (CommonMark allows bare inline
    /// text at the document root in some cases; we promote it).
    fn append_inline(&mut self, node: Node) -> Result<(), ParseError> {
        // Decide whether the current builder needs paragraph
        // wrapping BEFORE re-borrowing the stack mutably (avoids
        // the immutable-self-while-mutable-borrow conflict).
        let needs_wrap = matches!(self.block_stack.last(), Some(Block::Doc { .. }));
        let to_push = if needs_wrap {
            self.wrap_in_paragraph(node)?
        } else {
            node
        };
        match self.block_stack.last_mut() {
            Some(Block::Paragraph { children })
            | Some(Block::Heading { children, .. })
            | Some(Block::Item { children, .. })
            | Some(Block::Blockquote { children, .. })
            | Some(Block::Doc { children })
            | Some(Block::Custom { children, .. }) => {
                children.push(to_push);
                Ok(())
            }
            Some(Block::Image { .. }) | Some(Block::CodeBlock { .. }) => {
                // Image alt / code-block text already handled in
                // `append_text` — should never reach here for
                // those builders.
                Ok(())
            }
            Some(Block::List { .. }) | None => Err(ParseError::Empty),
        }
    }

    /// Append a finished block-level node to the current
    /// container. The container must accept block children (Doc,
    /// Blockquote, Item, List, Custom).
    fn append_block(&mut self, node: Node) -> Result<(), ParseError> {
        match self.block_stack.last_mut() {
            Some(Block::Doc { children })
            | Some(Block::Blockquote { children })
            | Some(Block::Custom { children, .. }) => {
                children.push(node);
                Ok(())
            }
            Some(Block::Item { children, .. }) => {
                children.push(node);
                Ok(())
            }
            Some(Block::List { items, .. }) => {
                items.push(node);
                Ok(())
            }
            Some(Block::Paragraph { children }) | Some(Block::Heading { children, .. }) => {
                // A block trying to close inside a paragraph /
                // heading is structurally weird (only `image` is
                // technically allowed); push as inline content.
                children.push(node);
                Ok(())
            }
            Some(Block::CodeBlock { .. }) | Some(Block::Image { .. }) => Ok(()),
            None => Err(ParseError::Empty),
        }
    }

    fn finish_block(&self, block: Block) -> Result<Node, ParseError> {
        match block {
            Block::Doc { children } => self
                .schema
                .node("doc", Attrs::new(), Fragment::from(children))
                .map_err(|e| ParseError::Schema {
                    node_type: "doc".into(),
                    error: e.to_string(),
                }),
            Block::Paragraph { children } => self
                .schema
                .node("paragraph", Attrs::new(), Fragment::from(children))
                .map_err(|e| ParseError::Schema {
                    node_type: "paragraph".into(),
                    error: e.to_string(),
                }),
            Block::Heading { level, children } => {
                let mut attrs = Attrs::new();
                attrs.insert("level".into(), json!(level.clamp(1, 6) as u8));
                self.schema
                    .node("heading", attrs, Fragment::from(children))
                    .map_err(|e| ParseError::Schema {
                        node_type: "heading".into(),
                        error: e.to_string(),
                    })
            }
            Block::Blockquote { children } => self
                .schema
                .node("blockquote", Attrs::new(), Fragment::from(children))
                .map_err(|e| ParseError::Schema {
                    node_type: "blockquote".into(),
                    error: e.to_string(),
                }),
            Block::CodeBlock { params, text } => {
                let mut attrs = Attrs::new();
                if !params.is_empty() {
                    attrs.insert("params".into(), json!(params));
                }
                let content = if text.is_empty() {
                    Fragment::empty()
                } else {
                    let text_node =
                        self.schema
                            .text(text, Vec::new())
                            .map_err(|e| ParseError::Schema {
                                node_type: "text".into(),
                                error: e.to_string(),
                            })?;
                    Fragment::from(text_node)
                };
                self.schema
                    .node("code_block", attrs, content)
                    .map_err(|e| ParseError::Schema {
                        node_type: "code_block".into(),
                        error: e.to_string(),
                    })
            }
            Block::List {
                ordered,
                start,
                items,
                is_task,
            } => {
                // Decide the effective list flavor BEFORE
                // normalizing items so we don't call
                // `schema.node("task_item", ...)` against a
                // schema that doesn't declare it — that would
                // surface as a `task_item` error and never reach
                // the documented bullet-list fallback. The
                // schema lookup determines whether the runtime
                // can express task lists at all.
                let task_supported = is_task
                    && self.schema.node_type("task_list").is_ok()
                    && self.schema.node_type("task_item").is_ok();
                if is_task && !task_supported {
                    tracing::warn!(
                        target: "pocopine.log",
                        "MarkdownParser: schema does not declare `task_list`/`task_item`; \
                         falling back to bullet_list."
                    );
                }
                let (list_type, item_type) = if task_supported {
                    ("task_list", "task_item")
                } else if ordered {
                    ("ordered_list", "list_item")
                } else {
                    ("bullet_list", "list_item")
                };

                let normalized_items: Vec<Node> = items
                    .into_iter()
                    .map(|item| self.normalize_item(item, item_type))
                    .collect::<Result<_, _>>()?;

                let mut attrs = Attrs::new();
                if !task_supported && ordered {
                    attrs.insert("order".into(), json!(start));
                }

                self.schema
                    .node(list_type, attrs, Fragment::from(normalized_items))
                    .map_err(|err| ParseError::Schema {
                        node_type: list_type.into(),
                        error: err.to_string(),
                    })
            }
            Block::Item { children, attrs } => {
                // The wrapping List builder decides whether this
                // becomes a list_item or task_item; here we just
                // package the children (auto-wrap loose inlines
                // in a paragraph) and pass through whatever attrs
                // accumulated during the item's scope (notably
                // `checked` written by the TaskListMarker handler
                // or by a Custom parse rule).
                let wrapped = self.wrap_loose_inlines_in_paragraph(children)?;
                self.schema
                    .node("list_item", attrs, Fragment::from(wrapped))
                    .map_err(|e| ParseError::Schema {
                        node_type: "list_item".into(),
                        error: e.to_string(),
                    })
            }
            Block::Image { src, title, alt } => {
                let mut attrs = Attrs::new();
                attrs.insert("src".into(), json!(src));
                if !title.is_empty() {
                    attrs.insert("title".into(), json!(title));
                }
                if !alt.is_empty() {
                    attrs.insert("alt".into(), json!(alt));
                }
                self.schema
                    .node("image", attrs, Fragment::empty())
                    .map_err(|e| ParseError::Schema {
                        node_type: "image".into(),
                        error: e.to_string(),
                    })
            }
            Block::Custom {
                node_type,
                attrs,
                children,
                source_tag: _,
                source_event: _,
            } => self
                .schema
                .node(&node_type, attrs, Fragment::from(children))
                .map_err(|e| ParseError::Schema {
                    node_type,
                    error: e.to_string(),
                }),
        }
    }

    /// Convert a generic `list_item`-shaped node into either a
    /// `list_item` or `task_item` depending on the list it's
    /// joining. Used by the List close-handler to retype items
    /// after we know the list flavor.
    ///
    /// We never early-return on a type-match: items are always
    /// built as `list_item` with a possibly-set `checked` attr
    /// (set by `TaskListMarker` events on the Item builder), so
    /// even when `target_type == "list_item"` we may need to
    /// strip the `checked` attr that doesn't belong on a plain
    /// list_item. Conversely, retyping to `task_item` may need
    /// to default `checked: false` for items the parser saw
    /// without a marker (rare; mixed-content task lists).
    fn normalize_item(&self, item: Node, target_type: &str) -> Result<Node, ParseError> {
        let mut attrs = item.attrs().clone();
        if target_type == "task_item" && !attrs.contains_key("checked") {
            attrs.insert("checked".into(), json!(false));
        }
        if target_type == "list_item" {
            attrs.remove("checked");
        }
        if item.type_name() == target_type && attrs == *item.attrs() {
            return Ok(item);
        }
        self.schema
            .node(target_type, attrs, item.content().clone())
            .map_err(|e| ParseError::Schema {
                node_type: target_type.into(),
                error: e.to_string(),
            })
    }

    /// Wrap a single inline node in a paragraph (`Schema::node`
    /// validates). Used by `append_inline` when promoting loose
    /// inline content at the document root.
    fn wrap_in_paragraph(&self, inline: Node) -> Result<Node, ParseError> {
        self.schema
            .node("paragraph", Attrs::new(), Fragment::from(inline))
            .map_err(|e| ParseError::Schema {
                node_type: "paragraph".into(),
                error: e.to_string(),
            })
    }

    /// Walk `children`: if all are block-level, return as-is. If
    /// any are inline (text / hard_break / image), wrap loose
    /// inline runs in paragraphs so the resulting fragment is
    /// schema-valid for `list_item`'s `paragraph block*` content.
    fn wrap_loose_inlines_in_paragraph(
        &self,
        children: Vec<Node>,
    ) -> Result<Vec<Node>, ParseError> {
        let mut out: Vec<Node> = Vec::with_capacity(children.len());
        let mut inline_buf: Vec<Node> = Vec::new();
        let flush =
            |buf: &mut Vec<Node>, out: &mut Vec<Node>, this: &Self| -> Result<(), ParseError> {
                if buf.is_empty() {
                    return Ok(());
                }
                let taken = std::mem::take(buf);
                let para = this
                    .schema
                    .node("paragraph", Attrs::new(), Fragment::from(taken))
                    .map_err(|e| ParseError::Schema {
                        node_type: "paragraph".into(),
                        error: e.to_string(),
                    })?;
                out.push(para);
                Ok(())
            };
        for child in children {
            if is_block_type(child.type_name()) {
                flush(&mut inline_buf, &mut out, self)?;
                out.push(child);
            } else {
                inline_buf.push(child);
            }
        }
        flush(&mut inline_buf, &mut out, self)?;
        // `list_item` requires `paragraph block*` per schema_basic.
        // For markdown shapes like `-\n  - child\n` (empty parent
        // item with a nested list as its first content) we'd
        // otherwise produce `[bullet_list]` and fail the
        // content-match. Prepend an empty paragraph when the first
        // element isn't already a paragraph.
        if out
            .first()
            .map(|n| n.type_name() != "paragraph")
            .unwrap_or(true)
        {
            let empty_para = self
                .schema
                .node("paragraph", Attrs::new(), Fragment::empty())
                .map_err(|e| ParseError::Schema {
                    node_type: "paragraph".into(),
                    error: e.to_string(),
                })?;
            out.insert(0, empty_para);
        }
        Ok(out)
    }

    fn code_mark(&self) -> Mark {
        Mark::new("code", Attrs::new())
    }

    fn finish(mut self) -> Result<Node, ParseError> {
        // Close any remaining open builders by appending an empty
        // paragraph if the doc would otherwise be empty (matches
        // CommonMark's minimal-valid-doc rule).
        if self.block_stack.len() != 1 {
            return Err(ParseError::Empty);
        }
        let root = self.block_stack.pop().unwrap();
        let Block::Doc { mut children } = root else {
            return Err(ParseError::Empty);
        };
        if children.is_empty() {
            let para = self
                .schema
                .node("paragraph", Attrs::new(), Fragment::empty())
                .map_err(|e| ParseError::Schema {
                    node_type: "paragraph".into(),
                    error: e.to_string(),
                })?;
            children.push(para);
        }
        self.schema
            .node("doc", Attrs::new(), Fragment::from(children))
            .map_err(|e| ParseError::Schema {
                node_type: "doc".into(),
                error: e.to_string(),
            })
    }
}

/// Classify an atomic `Event` (one that isn't Start/End) into
/// its [`EventKind`] discriminator for parse-rule lookup. Returns
/// `None` for Start/End events — those are routed to the
/// tag-rule lookup in `open_tag` / `close_tag`.
fn atomic_event_kind(event: &Event<'_>) -> Option<EventKind> {
    match event {
        Event::Code(_) => Some(EventKind::Code),
        Event::HardBreak => Some(EventKind::HardBreak),
        Event::SoftBreak => Some(EventKind::SoftBreak),
        Event::Rule => Some(EventKind::Rule),
        Event::TaskListMarker(_) => Some(EventKind::TaskListMarker),
        Event::InlineMath(_) => Some(EventKind::InlineMath),
        Event::DisplayMath(_) => Some(EventKind::DisplayMath),
        Event::FootnoteReference(_) => Some(EventKind::FootnoteReference),
        Event::Html(_) => Some(EventKind::Html),
        Event::InlineHtml(_) => Some(EventKind::InlineHtml),
        _ => None,
    }
}

/// Heuristic: is `type_name` a block-level type in schema_basic?
/// Used by [`ParseWalker::wrap_loose_inlines_in_paragraph`] to
/// decide whether a child needs paragraph-wrapping.
fn is_block_type(type_name: &str) -> bool {
    matches!(
        type_name,
        "paragraph"
            | "heading"
            | "blockquote"
            | "code_block"
            | "horizontal_rule"
            | "bullet_list"
            | "ordered_list"
            | "task_list"
            | "list_item"
            | "task_item"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attrs, Fragment};
    use serde_json::json;

    fn text(s: &str, marks: Vec<Mark>) -> Node {
        Node::unchecked(
            "text".to_string(),
            Attrs::new(),
            marks,
            Fragment::empty(),
            Some(s.to_string()),
            true,
        )
    }

    fn block(type_name: &str, attrs: Attrs, children: Vec<Node>) -> Node {
        Node::unchecked(
            type_name.to_string(),
            attrs,
            Vec::new(),
            Fragment::from(children),
            None,
            false,
        )
    }

    fn leaf(type_name: &str, attrs: Attrs) -> Node {
        Node::unchecked(
            type_name.to_string(),
            attrs,
            Vec::new(),
            Fragment::empty(),
            None,
            true,
        )
    }

    fn doc(children: Vec<Node>) -> Node {
        block("doc", Attrs::new(), children)
    }

    fn para_with_text(s: &str) -> Node {
        block("paragraph", Attrs::new(), vec![text(s, Vec::new())])
    }

    // ----- block types -----

    #[test]
    fn paragraph_round_trips_through_serializer() {
        let d = doc(vec![para_with_text("hello world")]);
        assert_eq!(serialize(&d).unwrap().trim(), "hello world");
    }

    #[test]
    fn two_paragraphs_get_blank_line_separator() {
        let d = doc(vec![para_with_text("one"), para_with_text("two")]);
        assert_eq!(serialize(&d).unwrap().trim(), "one\n\ntwo");
    }

    #[test]
    fn heading_emits_hash_prefix() {
        let mut attrs = Attrs::new();
        attrs.insert("level".into(), json!(2));
        let d = doc(vec![block(
            "heading",
            attrs,
            vec![text("Section", Vec::new())],
        )]);
        assert_eq!(serialize(&d).unwrap().trim(), "## Section");
    }

    #[test]
    fn heading_levels_1_through_6() {
        for level in 1u8..=6 {
            let mut attrs = Attrs::new();
            attrs.insert("level".into(), json!(level));
            let d = doc(vec![block("heading", attrs, vec![text("H", Vec::new())])]);
            let expected_prefix = "#".repeat(level as usize) + " H";
            assert_eq!(serialize(&d).unwrap().trim(), expected_prefix);
        }
    }

    #[test]
    fn blockquote_emits_gt_prefix() {
        let d = doc(vec![block(
            "blockquote",
            Attrs::new(),
            vec![para_with_text("quoted")],
        )]);
        let out = serialize(&d).unwrap();
        // `pulldown-cmark-to-cmark` renders blockquotes with a
        // leading-space padding (`" > "`) which is canonical
        // CommonMark too. Just verify the marker + text are both
        // present.
        assert!(out.contains(">"), "got: {out}");
        assert!(out.contains("quoted"), "got: {out}");
    }

    #[test]
    fn code_block_emits_fenced_block() {
        let mut attrs = Attrs::new();
        attrs.insert("params".into(), json!("rust"));
        let code = block("code_block", attrs, vec![text("fn x() {}", Vec::new())]);
        let d = doc(vec![code]);
        let out = serialize(&d).unwrap();
        // Library defaults to 4-backtick fences to support nested
        // code blocks — any fence length ≥ 3 is valid CommonMark,
        // so just check for the fence + lang + body.
        assert!(out.contains("```"), "got: {out}");
        assert!(out.contains("rust"), "got: {out}");
        assert!(out.contains("fn x() {}"), "got: {out}");
    }

    #[test]
    fn code_block_without_params_still_fences() {
        let code = block("code_block", Attrs::new(), vec![text("plain", Vec::new())]);
        let d = doc(vec![code]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("```"), "got: {out}");
        assert!(out.contains("plain"), "got: {out}");
    }

    #[test]
    fn horizontal_rule_emits_separator() {
        let d = doc(vec![
            para_with_text("before"),
            leaf("horizontal_rule", Attrs::new()),
            para_with_text("after"),
        ]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("before"), "got: {out}");
        assert!(out.contains("after"), "got: {out}");
        // CommonMark renders horizontal_rule as `---` (the default
        // marker pulldown-cmark-to-cmark chooses).
        assert!(
            out.contains("---") || out.contains("***") || out.contains("___"),
            "got: {out}"
        );
    }

    // ----- lists -----

    fn list_item_text(s: &str) -> Node {
        block("list_item", Attrs::new(), vec![para_with_text(s)])
    }

    #[test]
    fn bullet_list_emits_dash_or_star_items() {
        let d = doc(vec![block(
            "bullet_list",
            Attrs::new(),
            vec![list_item_text("a"), list_item_text("b")],
        )]);
        let out = serialize(&d).unwrap();
        // pulldown-cmark-to-cmark defaults to `*` for bullets.
        assert!(out.contains("* a"), "got: {out}");
        assert!(out.contains("* b"), "got: {out}");
    }

    #[test]
    fn ordered_list_emits_numbered_items() {
        let d = doc(vec![block(
            "ordered_list",
            Attrs::new(),
            vec![list_item_text("first"), list_item_text("second")],
        )]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("1. first"), "got: {out}");
        assert!(out.contains("2. second"), "got: {out}");
    }

    #[test]
    fn ordered_list_respects_start_attr() {
        let mut attrs = Attrs::new();
        attrs.insert("order".into(), json!(5));
        let d = doc(vec![block(
            "ordered_list",
            attrs,
            vec![list_item_text("five"), list_item_text("six")],
        )]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("5. five"), "got: {out}");
        assert!(out.contains("6. six"), "got: {out}");
    }

    // ----- marks -----

    #[test]
    fn em_mark_wraps_in_underscores_or_stars() {
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![
                text("hello ", Vec::new()),
                text("world", vec![Mark::new("em", Attrs::new())]),
            ],
        )]);
        let out = serialize(&d).unwrap();
        // pulldown-cmark-to-cmark emits `*world*` by default.
        assert!(out.contains("*world*"), "got: {out}");
        assert!(out.contains("hello"), "got: {out}");
    }

    #[test]
    fn strong_mark_wraps_in_double_stars() {
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![text("bold", vec![Mark::new("strong", Attrs::new())])],
        )]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("**bold**"), "got: {out}");
    }

    #[test]
    fn link_mark_emits_inline_link() {
        let mut href_attrs = Attrs::new();
        href_attrs.insert("href".into(), json!("https://example.com"));
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![text("anchor", vec![Mark::new("link", href_attrs)])],
        )]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("[anchor](https://example.com)"), "got: {out}");
    }

    #[test]
    fn link_mark_with_title_emits_title() {
        let mut href_attrs = Attrs::new();
        href_attrs.insert("href".into(), json!("https://example.com"));
        href_attrs.insert("title".into(), json!("Example"));
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![text("anchor", vec![Mark::new("link", href_attrs)])],
        )]);
        let out = serialize(&d).unwrap();
        assert!(
            out.contains("[anchor](https://example.com \"Example\")"),
            "got: {out}",
        );
    }

    #[test]
    fn code_mark_emits_backtick_span() {
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![text("inline", vec![Mark::new("code", Attrs::new())])],
        )]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("`inline`"), "got: {out}");
    }

    #[test]
    fn em_inside_strong_nests_correctly() {
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![text(
                "both",
                vec![
                    Mark::new("strong", Attrs::new()),
                    Mark::new("em", Attrs::new()),
                ],
            )],
        )]);
        let out = serialize(&d).unwrap();
        // pulldown-cmark-to-cmark renders this as either `***both***`
        // or `**_both_**` — either is valid CommonMark.
        assert!(
            out.contains("***both***")
                || out.contains("**_both_**")
                || out.contains("**both**") && out.contains("*both*"),
            "got: {out}",
        );
    }

    // ----- inline -----

    #[test]
    fn hard_break_emits_in_paragraph() {
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![
                text("line1", Vec::new()),
                leaf("hard_break", Attrs::new()),
                text("line2", Vec::new()),
            ],
        )]);
        let out = serialize(&d).unwrap();
        // pulldown-cmark-to-cmark emits a hard break as `  \n` or
        // `\\\n` — either way the result contains both lines.
        assert!(out.contains("line1"), "got: {out}");
        assert!(out.contains("line2"), "got: {out}");
        // The two lines should be on separate output lines.
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 2, "expected at least 2 lines, got: {out}");
    }

    #[test]
    fn image_emits_markdown_image() {
        let mut attrs = Attrs::new();
        attrs.insert("src".into(), json!("https://img.example/a.png"));
        attrs.insert("alt".into(), json!("alt-text"));
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![leaf("image", attrs)],
        )]);
        let out = serialize(&d).unwrap();
        assert!(
            out.contains("![alt-text](https://img.example/a.png)"),
            "got: {out}",
        );
    }

    #[test]
    fn special_characters_in_text_get_escaped() {
        let d = doc(vec![para_with_text("a*b_c[d]")]);
        let out = serialize(&d).unwrap();
        // Don't pin the exact escape style, but the literal must
        // round-trip — `*`, `_`, `[` `]` should not produce
        // emphasis / link parse on re-parse.
        assert!(out.contains("a"), "got: {out}");
        assert!(out.contains("b"), "got: {out}");
        // Verify escapes appear for at least the markdown-significant chars.
        assert!(out.contains("\\*") || out.contains("a\\*b"), "got: {out}");
    }

    // ----- node-emitter overrides -----

    #[test]
    fn register_node_intercepts_custom_type() {
        // A schema with a custom `callout` block — the override
        // wraps it in a blockquote with a `[!NOTE] ` prefix line.
        let callout = block("callout", Attrs::new(), vec![para_with_text("important")]);
        let d = doc(vec![callout]);

        let serializer = MarkdownSerializer::new().register_node(
            "callout",
            Arc::new(|node, _parent, _index, sink: &mut EventSink<'_>| {
                sink.push(Event::Start(Tag::BlockQuote(None)));
                sink.push(Event::Start(Tag::Paragraph));
                sink.push(Event::Text("[!NOTE]".to_string().into()));
                sink.push(Event::End(TagEnd::Paragraph));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::BlockQuote(None)));
            }),
        );

        let out = serializer.serialize(&d).unwrap();
        assert!(out.contains("[!NOTE]"), "got: {out}");
        assert!(out.contains("important"), "got: {out}");
        assert!(out.contains(">"), "got: {out}");
    }

    #[test]
    fn unknown_node_type_inlines_children_without_panicking() {
        // No `mystery` registered — default walker should inline
        // children (and log a warning, which we can't easily
        // assert here without a tracing test subscriber).
        let mystery = block("mystery", Attrs::new(), vec![para_with_text("survives")]);
        let d = doc(vec![mystery]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("survives"), "got: {out}");
    }

    // ----- the trickier doc -----

    // ----- Codex C1 round-2 regressions -----

    fn parse_back(md: &str) -> String {
        use pulldown_cmark::{Event as PEvent, Parser};
        Parser::new(md)
            .filter_map(|e| match e {
                PEvent::Text(t) => Some(t.to_string()),
                PEvent::Code(t) => Some(t.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn leading_special_char_does_not_double_escape() {
        // Codex C1 round 2 P1: `*todo` text used to pre-escape
        // to `\*todo`, then the renderer would add ITS first-char
        // escape on the resulting `\`, producing `\\*todo` —
        // which reparses with a literal backslash artifact.
        let d = doc(vec![para_with_text("*todo")]);
        let out = serialize(&d).unwrap();
        // Reparsing must give back the original text.
        assert_eq!(parse_back(&out), "*todo", "got: {out}");
    }

    #[test]
    fn leading_underscore_and_brackets_round_trip() {
        for input in ["_emphasis", "[bracket", "]bracket", "`code", "\\backslash"] {
            let d = doc(vec![para_with_text(input)]);
            let out = serialize(&d).unwrap();
            assert_eq!(parse_back(&out), input, "input={input}, got: {out}");
        }
    }

    #[test]
    fn double_star_at_start_round_trips() {
        // Two specials in a row: first handed off to renderer,
        // second pre-escaped by us. Without the fix the renderer
        // would build `**bold` from the leading `**`.
        let d = doc(vec![para_with_text("**bold")]);
        let out = serialize(&d).unwrap();
        assert_eq!(parse_back(&out), "**bold", "got: {out}");
    }

    #[test]
    fn code_block_with_four_backticks_uses_larger_fence() {
        // Codex C1 round 2 P1: fixed 4-backtick fence would close
        // prematurely inside a code block containing 4
        // consecutive backticks. The dynamic
        // `calculate_code_block_token_count` lets us emit a
        // 5-backtick fence around such content.
        let content = "before\n````\nafter";
        let code = block("code_block", Attrs::new(), vec![text(content, Vec::new())]);
        let d = doc(vec![code]);
        let out = serialize(&d).unwrap();
        // Reparse the result — the code-block contents must
        // round-trip exactly.
        use pulldown_cmark::{Event as PEvent, Parser, Tag as PTag, TagEnd as PTagEnd};
        let mut in_code = false;
        let mut got = String::new();
        for ev in Parser::new(&out) {
            match ev {
                PEvent::Start(PTag::CodeBlock(_)) => in_code = true,
                PEvent::End(PTagEnd::CodeBlock) => in_code = false,
                PEvent::Text(t) if in_code => got.push_str(&t),
                _ => {}
            }
        }
        assert_eq!(got.trim_end(), content, "got: {out}");
    }

    #[test]
    fn intra_word_underscore_is_not_escaped() {
        // CommonMark intra-word emphasis rule: `_` between two
        // word chars is literal text, not emphasis. The escape
        // function must respect this so `task_list` renders as
        // `task_list`, not `task\_list`.
        let d = doc(vec![para_with_text("task_list / snake_case_name")]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("task_list"), "got: {out}",);
        assert!(out.contains("snake_case_name"), "got: {out}",);
        assert!(
            !out.contains("\\_"),
            "intra-word underscores must not be escaped, got: {out}",
        );
        // Boundary underscores still need escapes.
        let d2 = doc(vec![para_with_text("_em_ and not _intra word_")]);
        let out2 = serialize(&d2).unwrap();
        // The leading `_em` and `_intra` underscores are word-
        // adjacent on one side but whitespace-adjacent on the
        // other → not intra-word → escaped.
        assert!(out2.contains("\\_em"), "got: {out2}");
    }

    #[test]
    fn backslash_followed_by_angle_bracket_round_trips() {
        // Codex C1 round 2 P2 follow-up: `\<a>` previously
        // round-tripped to literal `\` + InlineHtml("<a>")
        // because the renderer escaped the leading `\` but `<a>`
        // mid-line is a valid inline-HTML start. Adding `<` to
        // the inline escape set keeps the `<` literal after the
        // round-trip.
        let d = doc(vec![para_with_text("\\<a>")]);
        let out = serialize(&d).unwrap();
        assert_eq!(parse_back(&out), "\\<a>", "got: {out}");
    }

    #[test]
    fn inline_html_lookalike_text_round_trips() {
        // Bare `<a>` mid-paragraph would otherwise become inline
        // HTML on reparse. The `<` escape prevents that.
        let d = doc(vec![para_with_text("see <a> tag")]);
        let out = serialize(&d).unwrap();
        assert_eq!(parse_back(&out), "see <a> tag", "got: {out}");
    }

    #[test]
    fn image_alt_with_brackets_escapes_to_avoid_truncation() {
        // Codex C1 round 2 P2: unescaped `]` in alt would close
        // `![alt]` prematurely, producing `![a]b](url)` which
        // doesn't round-trip.
        let mut attrs = Attrs::new();
        attrs.insert("src".into(), json!("https://img.example/a.png"));
        attrs.insert("alt".into(), json!("a]b"));
        let d = doc(vec![block(
            "paragraph",
            Attrs::new(),
            vec![leaf("image", attrs)],
        )]);
        let out = serialize(&d).unwrap();
        // Parse back — the alt text must come back as `a]b`.
        use pulldown_cmark::{Event as PEvent, Parser, Tag as PTag, TagEnd as PTagEnd};
        let mut in_image = false;
        let mut alt = String::new();
        for ev in Parser::new(&out) {
            match ev {
                PEvent::Start(PTag::Image { .. }) => in_image = true,
                PEvent::End(PTagEnd::Image) => in_image = false,
                PEvent::Text(t) if in_image => alt.push_str(&t),
                _ => {}
            }
        }
        assert_eq!(alt, "a]b", "got: {out}");
    }

    #[test]
    fn nested_doc_round_trips_through_pulldown_cmark() {
        // doc:
        //   heading h2 "Recipe"
        //   bullet_list:
        //     - "flour"
        //     - "salt"
        //   paragraph: "And then bake at 350°F."
        let mut h2_attrs = Attrs::new();
        h2_attrs.insert("level".into(), json!(2));
        let d = doc(vec![
            block("heading", h2_attrs, vec![text("Recipe", Vec::new())]),
            block(
                "bullet_list",
                Attrs::new(),
                vec![list_item_text("flour"), list_item_text("salt")],
            ),
            para_with_text("And then bake at 350°F."),
        ]);
        let out = serialize(&d).unwrap();
        assert!(out.contains("## Recipe"), "got: {out}");
        assert!(out.contains("* flour"), "got: {out}");
        assert!(out.contains("* salt"), "got: {out}");
        assert!(out.contains("And then bake at 350°F."), "got: {out}");
    }

    // =====================================================================
    // Parser tests
    // =====================================================================

    fn schema() -> crate::model::Schema {
        crate::schema_basic::schema()
    }

    fn top_block_types(doc: &Node) -> Vec<String> {
        doc.content()
            .iter()
            .map(|n| n.type_name().to_string())
            .collect()
    }

    fn collect_text(node: &Node) -> String {
        let mut out = String::new();
        collect_text_into(node, &mut out);
        out
    }

    fn collect_text_into(node: &Node, out: &mut String) {
        if let Some(t) = node.text() {
            out.push_str(t);
            return;
        }
        for child in node.content().iter() {
            collect_text_into(child, out);
        }
    }

    fn first_descendant<'a>(node: &'a Node, type_name: &str) -> Option<&'a Node> {
        if node.type_name() == type_name {
            return Some(node);
        }
        for child in node.content().iter() {
            if let Some(found) = first_descendant(child, type_name) {
                return Some(found);
            }
        }
        None
    }

    fn count_descendants(node: &Node, type_name: &str) -> usize {
        let mut count = if node.type_name() == type_name { 1 } else { 0 };
        for child in node.content().iter() {
            count += count_descendants(child, type_name);
        }
        count
    }

    fn find_text_with_mark<'a>(node: &'a Node, mark_name: &str) -> Option<&'a Node> {
        if node.type_name() == "text" && node.marks().iter().any(|m| m.type_name() == mark_name) {
            return Some(node);
        }
        for child in node.content().iter() {
            if let Some(found) = find_text_with_mark(child, mark_name) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn parse_empty_string_yields_doc_with_empty_paragraph() {
        let s = schema();
        let doc = parse("", &s).unwrap();
        assert_eq!(doc.type_name(), "doc");
        assert_eq!(top_block_types(&doc), vec!["paragraph".to_string()]);
        assert!(
            doc.content()
                .child(0)
                .unwrap()
                .content()
                .iter()
                .next()
                .is_none()
        );
    }

    #[test]
    fn parse_simple_paragraph() {
        let s = schema();
        let doc = parse("hello world", &s).unwrap();
        assert_eq!(top_block_types(&doc), vec!["paragraph"]);
        assert_eq!(collect_text(&doc), "hello world");
    }

    #[test]
    fn parse_two_paragraphs() {
        let s = schema();
        let doc = parse("one\n\ntwo", &s).unwrap();
        assert_eq!(top_block_types(&doc), vec!["paragraph", "paragraph"]);
        assert_eq!(collect_text(&doc), "onetwo");
    }

    #[test]
    fn parse_heading_levels() {
        let s = schema();
        for level in 1..=6 {
            let md = format!("{} H", "#".repeat(level));
            let doc = parse(&md, &s).unwrap();
            let h = first_descendant(&doc, "heading").expect("heading");
            assert_eq!(
                h.attrs().get("level").and_then(|v| v.as_u64()),
                Some(level as u64),
            );
        }
    }

    #[test]
    fn parse_blockquote() {
        let s = schema();
        let doc = parse("> quoted text", &s).unwrap();
        assert_eq!(top_block_types(&doc), vec!["blockquote"]);
        assert_eq!(collect_text(&doc), "quoted text");
    }

    #[test]
    fn parse_fenced_code_block_with_language() {
        let s = schema();
        let doc = parse("```rust\nfn x() {}\n```\n", &s).unwrap();
        let cb = first_descendant(&doc, "code_block").expect("code_block");
        assert_eq!(
            cb.attrs().get("params").and_then(|v| v.as_str()),
            Some("rust"),
        );
        assert_eq!(collect_text(cb), "fn x() {}\n");
    }

    #[test]
    fn parse_bullet_list_items() {
        let s = schema();
        let doc = parse("* a\n* b\n", &s).unwrap();
        let list = first_descendant(&doc, "bullet_list").expect("bullet_list");
        assert_eq!(list.child_count(), 2);
        assert_eq!(collect_text(list), "ab");
    }

    #[test]
    fn parse_ordered_list_preserves_start() {
        let s = schema();
        let doc = parse("3. third\n4. fourth\n", &s).unwrap();
        let list = first_descendant(&doc, "ordered_list").expect("ordered_list");
        assert_eq!(list.attrs().get("order").and_then(|v| v.as_u64()), Some(3),);
        assert_eq!(list.child_count(), 2);
    }

    #[test]
    fn parse_horizontal_rule() {
        let s = schema();
        let doc = parse("before\n\n---\n\nafter", &s).unwrap();
        assert_eq!(count_descendants(&doc, "horizontal_rule"), 1);
    }

    #[test]
    fn parse_em_and_strong() {
        let s = schema();
        let doc = parse("*italic* and **bold**", &s).unwrap();
        assert!(find_text_with_mark(&doc, "em").is_some(), "em mark missing");
        assert!(
            find_text_with_mark(&doc, "strong").is_some(),
            "strong mark missing"
        );
    }

    #[test]
    fn parse_link_mark_captures_href_and_title() {
        let s = schema();
        let doc = parse("see [anchor](https://example.com \"Example\") here", &s).unwrap();
        let link_text = find_text_with_mark(&doc, "link").expect("link mark");
        let link_mark = link_text
            .marks()
            .iter()
            .find(|m| m.type_name() == "link")
            .expect("link mark instance");
        assert_eq!(
            link_mark.attrs().get("href").and_then(|v| v.as_str()),
            Some("https://example.com"),
        );
        assert_eq!(
            link_mark.attrs().get("title").and_then(|v| v.as_str()),
            Some("Example"),
        );
        assert_eq!(link_text.text(), Some("anchor"));
    }

    #[test]
    fn parse_inline_code() {
        let s = schema();
        let doc = parse("use `foo` here", &s).unwrap();
        let code_text = find_text_with_mark(&doc, "code").expect("code mark");
        assert_eq!(code_text.text(), Some("foo"));
    }

    #[test]
    fn parse_hard_break() {
        let s = schema();
        // Two-space + newline is the CommonMark hard-break form.
        let doc = parse("line1  \nline2", &s).unwrap();
        assert_eq!(count_descendants(&doc, "hard_break"), 1);
    }

    #[test]
    fn parse_image() {
        let s = schema();
        let doc = parse("![alt-text](https://img.example/a.png)", &s).unwrap();
        let img = first_descendant(&doc, "image").expect("image");
        assert_eq!(
            img.attrs().get("src").and_then(|v| v.as_str()),
            Some("https://img.example/a.png"),
        );
        assert_eq!(
            img.attrs().get("alt").and_then(|v| v.as_str()),
            Some("alt-text"),
        );
    }

    #[test]
    fn parse_task_list_with_checked_and_unchecked() {
        let s = schema();
        let md = "- [x] done\n- [ ] todo\n";
        let doc = parse(md, &s).unwrap();
        let list = first_descendant(&doc, "task_list").expect("task_list");
        assert_eq!(list.child_count(), 2);
        let first = list.content().child(0).unwrap();
        assert_eq!(first.type_name(), "task_item");
        assert_eq!(
            first.attrs().get("checked").and_then(|v| v.as_bool()),
            Some(true),
        );
        let second = list.content().child(1).unwrap();
        assert_eq!(
            second.attrs().get("checked").and_then(|v| v.as_bool()),
            Some(false),
        );
    }

    // ----- round-trip tests -----

    fn assert_round_trips(md: &str) {
        let s = schema();
        let parsed = parse(md, &s).unwrap();
        let serialized = serialize(&parsed).unwrap();
        // Reparsing the serialized output must produce the SAME
        // structure (modulo whitespace / fence width). We compare
        // by re-serializing the round-tripped doc and asserting
        // the second-generation output equals the first.
        let reparsed = parse(&serialized, &s).unwrap();
        let reserialized = serialize(&reparsed).unwrap();
        assert_eq!(
            serialized, reserialized,
            "round-trip not idempotent for input: {md:?}\n\
             first serialize:\n{serialized}\n\
             second serialize:\n{reserialized}",
        );
    }

    #[test]
    fn round_trip_paragraph() {
        assert_round_trips("hello world");
    }

    #[test]
    fn round_trip_heading_and_paragraph() {
        assert_round_trips("# Title\n\nSome body text.");
    }

    #[test]
    fn round_trip_em_and_strong() {
        assert_round_trips("a *b* c **d** e");
    }

    #[test]
    fn round_trip_bullet_list() {
        assert_round_trips("* a\n* b\n* c\n");
    }

    #[test]
    fn round_trip_ordered_list() {
        assert_round_trips("1. first\n2. second\n3. third\n");
    }

    #[test]
    fn round_trip_task_list() {
        assert_round_trips("- [x] done\n- [ ] todo\n");
    }

    #[test]
    fn round_trip_blockquote_with_inline_marks() {
        assert_round_trips("> *italic* in a quote");
    }

    #[test]
    fn round_trip_inline_link() {
        assert_round_trips("see [anchor](https://example.com) here");
    }

    #[test]
    fn round_trip_inline_code() {
        assert_round_trips("use `fn foo()` to call");
    }

    #[test]
    fn round_trip_fenced_code_block() {
        assert_round_trips("```rust\nfn x() {}\n```\n");
    }

    #[test]
    fn round_trip_image() {
        assert_round_trips("![alt](https://img.example/a.png)");
    }

    #[test]
    fn round_trip_nested_blockquote_and_list() {
        assert_round_trips("> * a\n> * b\n> * c\n");
    }

    // ----- Codex C3 round-2 regressions -----

    #[test]
    fn image_alt_preserves_code_span_text() {
        // Codex C3 round 2 P2: `![a `b` c](url)` — the code span
        // inside the alt previously dropped (`alt = "a  c"`)
        // because `Event::Code` created a text node that
        // `append_inline`'s Block::Image arm silently swallowed.
        // The fix routes Code events to the alt collector while
        // inside an Image builder.
        let s = schema();
        let doc = parse("![a `b` c](url)", &s).unwrap();
        let img = first_descendant(&doc, "image").expect("image");
        assert_eq!(
            img.attrs().get("alt").and_then(|v| v.as_str()),
            Some("a b c"),
            "code span text must survive inside image alt",
        );
    }

    #[test]
    fn empty_list_item_with_nested_list_does_not_reject() {
        // Codex C3 round 2 P2: `-\n  - child\n` — the outer item
        // has no inline content, just a nested bullet list. The
        // schema requires `paragraph block*` for list_item, so
        // we must synthesize an empty paragraph at position 0.
        let s = schema();
        let doc = parse("-\n  - child\n", &s).unwrap();
        let outer_list = first_descendant(&doc, "bullet_list").expect("outer bullet_list");
        let outer_item = outer_list.content().child(0).expect("outer item");
        let first_child = outer_item.content().child(0).expect("first child");
        assert_eq!(
            first_child.type_name(),
            "paragraph",
            "outer item must start with a (possibly empty) paragraph",
        );
        // The nested list survives as the second child.
        let second_child = outer_item.content().child(1).expect("second child");
        assert_eq!(second_child.type_name(), "bullet_list");
    }

    #[test]
    fn task_list_fallback_strips_checked_attr_from_list_items() {
        // Codex C3 round 3: when the task-fallback path produces
        // a `bullet_list`, the items must NOT carry a leftover
        // `checked` attribute from the original TaskListMarker
        // events. `list_item` doesn't declare `checked` and
        // smuggling it in produces semantically wrong documents
        // (the attr survives but no UI / serializer renders it).
        use crate::extensions::{
            CoreInlineExtension, CoreMarksExtension, CoreNodesExtension, ListsExtension,
        };
        use crate::runtime::RuntimeBuilder;

        let rt = RuntimeBuilder::new()
            .without_defaults()
            .with(CoreNodesExtension)
            .with(CoreInlineExtension)
            .with(CoreMarksExtension)
            .with(ListsExtension)
            .build();
        let s = rt.schema();
        let doc = parse("- [x] done\n- [ ] todo\n", s).unwrap();
        let bullet = first_descendant(&doc, "bullet_list").expect("bullet_list");
        for i in 0..bullet.child_count() {
            let item = bullet.content().child(i).unwrap();
            assert_eq!(item.type_name(), "list_item");
            assert!(
                !item.attrs().contains_key("checked"),
                "fallback list_item must not carry `checked`, got attrs: {:?}",
                item.attrs(),
            );
        }
    }

    #[test]
    fn task_list_against_schema_without_task_types_falls_back() {
        // Codex C3 round 2 P2: a schema with bullet_list but no
        // task_list / task_item types must produce a bullet_list,
        // not a `task_item` schema error. The fix decides task
        // support BEFORE normalizing items.
        use crate::extensions::{
            CoreInlineExtension, CoreMarksExtension, CoreNodesExtension, ListsExtension,
        };
        use crate::runtime::RuntimeBuilder;

        let rt = RuntimeBuilder::new()
            .without_defaults()
            .with(CoreNodesExtension)
            .with(CoreInlineExtension)
            .with(CoreMarksExtension)
            .with(ListsExtension)
            .build();
        let s = rt.schema();
        // Schema must have bullet_list but NOT task_list.
        assert!(s.node_type("bullet_list").is_ok());
        assert!(s.node_type("task_list").is_err());

        let doc = parse("- [x] done\n- [ ] todo\n", s).expect("parses without task schema");
        // Should produce a bullet_list (fallback).
        let bullet = first_descendant(&doc, "bullet_list").expect("bullet_list fallback");
        assert_eq!(bullet.child_count(), 2);
    }
}
