//! `pocopine lsp` — language server for `.poco` templates, over stdio.
//!
//! This is the Rust-backed counterpart to the TypeScript server in the
//! `vscode-poco` extension. The strategic point (RFC-012 / RFC-050): reuse the
//! framework's *own* parsers instead of re-implementing them in TypeScript,
//! which is what causes the editor tooling to drift from the compiler. The
//! `#[component]` macro and this server both call
//! [`pocopine_template_parser::parse`], so a template that errors in the editor
//! errors the same way at compile time.
//!
//! Scope today: full-document sync + structural template diagnostics
//! (framework-owned, span-anchored). Deliberately minimal — completion, hover,
//! expression diagnostics (via `pocopine-expr`) and Stylekit unknown-utility
//! diagnostics (via `pocopine-stylekit`'s `compile_project`) are the next
//! layers and are noted at their call sites.

use std::collections::{HashMap, HashSet};
use std::ops::Range as ByteRange;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use pocopine_stylekit::{CompileOptions, Severity, SourceFile, compile_project};
use tokio::sync::Mutex;
use tower_lsp::lsp_types::Range as LspRange;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::args::LspArgs;
use crate::component_index::{self, ComponentIndex, FieldRole};

/// Blocking entry point invoked from `main`. Spins a tokio runtime and serves
/// the language server on stdin/stdout until the client disconnects.
pub fn run(_args: LspArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve());
    Ok(())
}

async fn serve() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

struct Backend {
    client: Client,
    /// Open document text, keyed by URI. Kept for the completion/hover layers
    /// that will need synchronous access to source between notifications.
    docs: Mutex<HashMap<Url, String>>,
    /// Cached Stylekit theme for the last-seen project: `(project_root, theme)`,
    /// where the inner `None` means Stylekit is disabled for that project. Reads
    /// the project config + `@theme` CSS once, not on every keystroke. (Editing
    /// the theme CSS needs a window reload to refresh — acceptable for now.)
    stylekit: Mutex<Option<(PathBuf, Option<String>)>>,
    /// Cached `#[component]` index for the last-seen project, keyed by its root.
    /// Scanned once (adding/renaming a component needs a window reload to pick
    /// up — acceptable for now; a future `.rs` watcher would invalidate it).
    components: Mutex<Option<(PathBuf, Arc<ComponentIndex>)>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            docs: Mutex::new(HashMap::new()),
            stylekit: Mutex::new(None),
            components: Mutex::new(None),
        }
    }

    /// Resolve (and cache) the `#[component]` index for the project owning `path`.
    async fn component_index(&self, project: &Path) -> Arc<ComponentIndex> {
        let mut cache = self.components.lock().await;
        if let Some((root, index)) = cache.as_ref()
            && root == project
        {
            return Arc::clone(index);
        }
        let index = Arc::new(component_index::scan(project));
        *cache = Some((project.to_path_buf(), Arc::clone(&index)));
        index
    }

    /// Symbol-aware diagnostics: bare field/handler identifiers in directive
    /// values that the owning component doesn't declare, and cross-component
    /// `:prop` / `pp-model:prop` bindings that target a missing or `state`-role
    /// field on a child. Mirrors the TypeScript server's `checkUnknownIdentifiers`
    /// + `checkChildBindingTargets`, backed by the Rust `#[component]` index.
    async fn symbol_diagnostics(
        &self,
        uri: &Url,
        ast: &pocopine_template_parser::TemplateAst,
        text: &str,
        index: &LineIndex<'_>,
    ) -> Vec<Diagnostic> {
        let Ok(doc_path) = uri.to_file_path() else {
            return Vec::new();
        };
        let Some(project) = find_project_root(&doc_path) else {
            return Vec::new();
        };
        let components = self.component_index(&project).await;

        // The component this `.poco` belongs to, by the filename convention
        // (`Counter.poco` ↔ `Counter`). Absent → skip self-identifier checks.
        let own = doc_path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|stem| components.by_name(stem));

        // Loop variables are template-wide here (no lexical scoping) — errs
        // toward silence, matching the TS server.
        let mut loop_vars: HashSet<String> = HashSet::new();
        collect_loop_vars(ast, &mut loop_vars);

        let mut out = Vec::new();
        check_symbols(ast, text, index, &components, own, &loop_vars, &mut out);
        out
    }

    /// `(component index, this document's own component name)` for `uri`.
    async fn project_context(&self, uri: &Url) -> (Option<Arc<ComponentIndex>>, Option<String>) {
        let Ok(doc_path) = uri.to_file_path() else {
            return (None, None);
        };
        let Some(root) = find_project_root(&doc_path) else {
            return (None, None);
        };
        let index = self.component_index(&root).await;
        let own = doc_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from);
        (Some(index), own)
    }

    async fn completions(&self, uri: &Url, text: &str, pos: Position) -> Vec<CompletionItem> {
        let line_index = LineIndex::new(text);
        let offset = line_index.offset_at(pos);
        let before = &text[..offset];
        let ctx = detect_context(before);
        if matches!(ctx, CompletionCtx::None) {
            return Vec::new();
        }

        let (index, own) = self.project_context(uri).await;
        let index = index.as_deref();
        let own = own.as_deref();

        match ctx {
            CompletionCtx::TagName => index.map(tag_completions).unwrap_or_default(),
            CompletionCtx::AttributeName => directive_name_completions(),
            CompletionCtx::DirectiveArg(head) => directive_arg_completions(&head),
            CompletionCtx::DirectiveModifier(head) => directive_modifier_completions(&head),
            CompletionCtx::TransitionPreset => transition_preset_completions(),
            CompletionCtx::FieldValue => own_fields(index, own),
            CompletionCtx::HandlerValue => own_handlers(index, own),
            CompletionCtx::Interpolation => {
                let mut items = magic_completions();
                items.extend(own_fields(index, own));
                items
            }
            CompletionCtx::ClassValue => class_completions(),
            CompletionCtx::None => Vec::new(),
        }
    }

    async fn hover_at(&self, uri: &Url, text: &str, pos: Position) -> Option<Hover> {
        let line_index = LineIndex::new(text);
        let (token, _) = token_at(line_index.line_text(pos.line), pos.character as usize)?;

        // 1. A directive attribute name → its registry doc.
        if let Some(parsed) = pocopine_directives::parse_directive_attr(&token) {
            if let Some(spec) = pocopine_directives::lookup(&parsed.head) {
                let mut md = spec.hover_doc.to_string();
                if let Some(rfc) = spec.rfc {
                    md.push_str(&format!("\n\n_{rfc}_"));
                }
                return Some(hover_md(md));
            }
            if let Some(msg) = pocopine_directives::removed_message(&parsed.head) {
                return Some(hover_md(msg.to_string()));
            }
        }

        // 2. A component tag, or a field/handler of this document's component.
        let (index, own) = self.project_context(uri).await;
        let index = index?;
        if let Some(comp) = index.by_tag(&token) {
            return Some(hover_md(component_contract_md(comp)));
        }
        if let Some(comp) = own.as_deref().and_then(|n| index.by_name(n)) {
            if let Some(field) = comp.field(&token) {
                return Some(hover_md(format!(
                    "**{}** — {} field on `{}`.",
                    field.name,
                    role_str(field.role),
                    comp.name
                )));
            }
            if comp.has_handler(&token) {
                return Some(hover_md(format!(
                    "**{token}** — handler on `{}`.",
                    comp.name
                )));
            }
        }
        None
    }

    async fn goto(&self, uri: &Url, text: &str, pos: Position) -> Option<Location> {
        let line_index = LineIndex::new(text);
        let (token, _) = token_at(line_index.line_text(pos.line), pos.character as usize)?;

        let (index, own) = self.project_context(uri).await;
        let index = index?;

        // Component tag → struct declaration.
        if let Some(comp) = index.by_tag(&token) {
            return location(&comp.file, comp.pos);
        }
        // Field / handler of this document's component → its declaration.
        if let Some(comp) = own.as_deref().and_then(|n| index.by_name(n)) {
            if let Some(field) = comp.field(&token) {
                return location(&comp.file, field.pos);
            }
            if let Some(handler) = comp.handler(&token) {
                return location(&handler.file, handler.pos);
            }
        }
        None
    }

    /// Re-parse a document and publish its diagnostics. Parses + indexes lines
    /// once, then runs every pass against the shared AST: structural template
    /// errors, directive validation, `{{expr}}` interpolation, Stylekit class
    /// checks, and symbol-aware (`#[component]` field/handler) checks.
    async fn refresh(&self, uri: Url, text: String) {
        let index = LineIndex::new(&text);
        let (ast, parse_errors) = pocopine_template_parser::parse(&text, "<editor>");

        let mut diagnostics = structural_diagnostics(parse_errors, &index);
        diagnostics.extend(directive_diagnostics(&ast, &text, &index));
        diagnostics.extend(interpolation_diagnostics(&text, &index));
        diagnostics.extend(self.stylekit_diagnostics(&uri, &text).await);
        diagnostics.extend(self.symbol_diagnostics(&uri, &ast, &text, &index).await);

        self.docs.lock().await.insert(uri.clone(), text);
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    /// Pine Stylekit (RFC-092) class diagnostics for one open document, via the
    /// shared `pocopine-stylekit` compiler — the same registry, theme tokens,
    /// and known-component-classes the build uses, so there are no false
    /// positives on project-defined color tokens or author CSS classes. Inert
    /// (empty) when Stylekit isn't enabled for the project.
    async fn stylekit_diagnostics(&self, uri: &Url, text: &str) -> Vec<Diagnostic> {
        let Ok(doc_path) = uri.to_file_path() else {
            return Vec::new();
        };
        let Some(project) = find_project_root(&doc_path) else {
            return Vec::new();
        };

        // Resolve (and cache) the project's theme CSS.
        let theme = {
            let mut cache = self.stylekit.lock().await;
            match cache.as_ref() {
                Some((p, t)) if *p == project => t.clone(),
                _ => {
                    let t = crate::stylekit::project_theme_css(&project);
                    *cache = Some((project.clone(), t.clone()));
                    t
                }
            }
        };
        let Some(theme_css) = theme else {
            return Vec::new(); // Stylekit not enabled here
        };

        let files = vec![SourceFile {
            path: doc_path,
            source: text.to_string(),
        }];
        let out = compile_project(&theme_css, &files, CompileOptions::default());
        let index = LineIndex::new(text);

        out.diagnostics
            .into_iter()
            // Keep only this document's class diagnostics with a real span.
            // Drop: template parse errors (emitted already by the structural
            // pass) and the build-time "dynamic class binding" note, both noise
            // in an editor.
            .filter(|d| {
                d.span.file == 0
                    && d.span.end > d.span.start
                    && !d.message.starts_with("parse error")
                    && !d.message.starts_with("dynamic class binding")
            })
            .map(|d| {
                let severity = match d.severity {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                };
                let message = match &d.help {
                    Some(help) => format!("{} ({})", d.message, help),
                    None => d.message.clone(),
                };
                let range = index.range(&(d.span.start as usize..d.span.end as usize));
                diag(range, severity, message)
            })
            .collect()
    }
}

/// Nearest ancestor directory of `start` that contains a `Cargo.toml` — the
/// crate the `.poco` belongs to, which is where its pocopine config lives.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file())
        .map(|dir| dir.to_path_buf())
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "pocopine-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                // Full sync keeps the scaffold simple: every change ships the
                // whole document, so we always re-parse from a known-good text.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(
                        ["<", "p", ":", "@", ".", "\"", "'", "{", "$", "-"]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    ),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "pocopine language server ready")
            .await;

        // Watch `.rs` (component index) and `.css` (Stylekit theme) so edits to
        // either invalidate the caches without a window reload.
        let options = DidChangeWatchedFilesRegistrationOptions {
            watchers: vec![
                FileSystemWatcher {
                    glob_pattern: GlobPattern::String("**/*.rs".to_string()),
                    kind: None,
                },
                FileSystemWatcher {
                    glob_pattern: GlobPattern::String("**/*.css".to_string()),
                    kind: None,
                },
            ],
        };
        let registration = Registration {
            id: "pocopine-watch-sources".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: serde_json::to_value(options).ok(),
        };
        let _ = self.client.register_capability(vec![registration]).await;
    }

    async fn did_change_watched_files(&self, _params: DidChangeWatchedFilesParams) {
        // A `.rs` or `.css` changed: drop the cached component index + Stylekit
        // theme so the next pass rescans, then re-validate every open document
        // against the fresh data.
        *self.components.lock().await = None;
        *self.stylekit.lock().await = None;

        let open: Vec<(Url, String)> = self
            .docs
            .lock()
            .await
            .iter()
            .map(|(u, t)| (u.clone(), t.clone()))
            .collect();
        for (uri, text) in open {
            self.refresh(uri, text).await;
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.refresh(doc.uri, doc.text).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        // FULL sync → the last content change carries the entire new document.
        if let Some(change) = params.content_changes.pop() {
            self.refresh(params.text_document.uri, change.text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.lock().await.remove(&uri);
        // Clear diagnostics for a closed file.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let pos = params.text_document_position;
        let text = self.docs.lock().await.get(&pos.text_document.uri).cloned();
        let Some(text) = text else {
            return Ok(None);
        };
        let items = self
            .completions(&pos.text_document.uri, &text, pos.position)
            .await;
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let p = params.text_document_position_params;
        let text = self.docs.lock().await.get(&p.text_document.uri).cloned();
        let Some(text) = text else {
            return Ok(None);
        };
        Ok(self.hover_at(&p.text_document.uri, &text, p.position).await)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let p = params.text_document_position_params;
        let text = self.docs.lock().await.get(&p.text_document.uri).cloned();
        let Some(text) = text else {
            return Ok(None);
        };
        Ok(self
            .goto(&p.text_document.uri, &text, p.position)
            .await
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<PrepareRenameResponse>> {
        let text = self
            .docs
            .lock()
            .await
            .get(&params.text_document.uri)
            .cloned();
        let Some(text) = text else {
            return Ok(None);
        };
        // Rename is offered only for `pp-for` loop variables — they're fully
        // `.poco`-local, so renaming them can't desync the Rust side. Fields,
        // handlers, and component tags need rust-analyzer (the `node` server),
        // so we decline them here rather than risk a broken `.rs`.
        let index = LineIndex::new(&text);
        let Some((ident, start)) = ident_at(
            index.line_text(params.position.line),
            params.position.character as usize,
        ) else {
            return Ok(None);
        };
        let (ast, _) = pocopine_template_parser::parse(&text, "<editor>");
        let mut loop_vars = HashSet::new();
        collect_loop_vars(&ast, &mut loop_vars);
        if !loop_vars.contains(&ident) {
            return Ok(None);
        }
        let p = params.position;
        let range = LspRange::new(
            Position::new(p.line, start as u32),
            Position::new(p.line, start as u32 + ident.chars().count() as u32),
        );
        Ok(Some(PrepareRenameResponse::Range(range)))
    }

    async fn rename(
        &self,
        params: RenameParams,
    ) -> tower_lsp::jsonrpc::Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let text = self.docs.lock().await.get(&uri).cloned();
        let Some(text) = text else {
            return Ok(None);
        };
        let index = LineIndex::new(&text);
        let Some((old, _)) = ident_at(index.line_text(pos.line), pos.character as usize) else {
            return Ok(None);
        };
        let (ast, _) = pocopine_template_parser::parse(&text, "<editor>");
        let mut loop_vars = HashSet::new();
        collect_loop_vars(&ast, &mut loop_vars);
        if !loop_vars.contains(&old) {
            return Ok(None); // safety net; prepare_rename already gated this
        }

        let edits: Vec<TextEdit> = loop_var_occurrences(&ast, &text, &index, &old)
            .into_iter()
            .map(|range| TextEdit {
                range,
                new_text: params.new_name.clone(),
            })
            .collect();
        if edits.is_empty() {
            return Ok(None);
        }
        let mut changes = HashMap::new();
        changes.insert(uri, edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> tower_lsp::jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let text = self
            .docs
            .lock()
            .await
            .get(&params.text_document.uri)
            .cloned();
        let Some(text) = text else {
            return Ok(None);
        };
        let index = LineIndex::new(&text);
        let (ast, _) = pocopine_template_parser::parse(&text, "<editor>");
        let symbols = outline(&ast, &index);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

fn diag(range: LspRange, severity: DiagnosticSeverity, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(severity),
        source: Some("pocopine".to_string()),
        message,
        ..Default::default()
    }
}

/// Structural diagnostics from the shared template parser.
///
/// `parse` returns *every* observation; we keep only framework-owned errors
/// (anchored byte ranges) and drop html5ever spec-recovery notices, whose
/// `byte_range` is `0..0` — the same rule `parse_strict` applies before it
/// gates compilation.
fn structural_diagnostics(
    errors: Vec<pocopine_template_parser::ParseError>,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    errors
        .into_iter()
        .filter(|e| !(e.byte_range.start == 0 && e.byte_range.end == 0))
        .map(|e| {
            diag(
                index.range(&e.byte_range),
                DiagnosticSeverity::ERROR,
                e.message,
            )
        })
        .collect()
}

/// Directive diagnostics from the shared registry ([`pocopine_directives`]) —
/// the same source of truth the `#[component]` macro's RFC-063 scan reads, so
/// "removed directive" / "unknown directive" / arg + host-constraint errors
/// match the compiler. Walks the parsed AST and anchors each diagnostic on the
/// offending attribute name within its element's opening tag.
fn directive_diagnostics(
    ast: &pocopine_template_parser::TemplateAst,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    use pocopine_template_parser::Node;
    let mut out = Vec::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk_directives(el, text, index, &mut out);
        }
    }
    out
}

fn walk_directives(
    el: &pocopine_template_parser::Element,
    text: &str,
    index: &LineIndex,
    out: &mut Vec<Diagnostic>,
) {
    use pocopine_directives::{
        ArgReq, Host, is_component_host, lookup, parse_directive_attr, removed_message,
    };
    use pocopine_template_parser::Node;

    // Synthetic (html5ever-inserted) elements carry no authored attributes —
    // skip their attrs, still recurse into children.
    if !el.synthetic {
        for (attr_name, _value) in &el.attrs {
            let Some(parsed) = parse_directive_attr(attr_name) else {
                continue; // plain HTML attribute
            };
            if parsed.head.is_empty() {
                continue; // bare `:` / `@` — value-completion concern, not here
            }
            let range = attr_name_range(el, attr_name, text, index);

            if let Some(message) = removed_message(&parsed.head) {
                out.push(diag(range, DiagnosticSeverity::ERROR, message.to_string()));
                continue;
            }

            let Some(spec) = lookup(&parsed.head) else {
                out.push(diag(
                    range,
                    DiagnosticSeverity::ERROR,
                    format!("Unknown pocopine directive '{attr_name}'."),
                ));
                continue;
            };

            // Arg presence.
            let has_arg = parsed
                .arg
                .as_deref()
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            match spec.takes_arg {
                ArgReq::Required if !has_arg => out.push(diag(
                    range,
                    DiagnosticSeverity::ERROR,
                    format!("pp-{} requires an arg after ':'.", spec.name),
                )),
                ArgReq::Forbidden if parsed.arg.is_some() => out.push(diag(
                    range,
                    DiagnosticSeverity::ERROR,
                    format!(
                        "pp-{} does not take an arg (got ':{}').",
                        spec.name,
                        parsed.arg.as_deref().unwrap_or("")
                    ),
                )),
                _ => {}
            }

            // Host-element constraint.
            match spec.host {
                Host::TemplateOnly if el.tag != "template" => out.push(diag(
                    range,
                    DiagnosticSeverity::ERROR,
                    format!(
                        "pp-{} requires a <template> host (got <{}>).",
                        spec.name, el.tag
                    ),
                )),
                Host::ComponentOnly if !is_component_host(&el.tag) => out.push(diag(
                    range,
                    DiagnosticSeverity::ERROR,
                    format!(
                        "pp-{} is only valid on component tags or <root> (got <{}>).",
                        spec.name, el.tag
                    ),
                )),
                _ => {}
            }
        }
    }

    for child in &el.children {
        if let Node::Element(child_el) = child {
            walk_directives(child_el, text, index, out);
        }
    }
}

/// Best-effort byte range of an attribute *name* for diagnostic anchoring.
/// Prefers a search within the element's opening tag (precise); if that range
/// is the `0..0` placeholder or doesn't contain the name, falls back to the
/// first whole-document occurrence; failing that, the document start.
fn attr_name_range(
    el: &pocopine_template_parser::Element,
    attr_name: &str,
    text: &str,
    index: &LineIndex,
) -> LspRange {
    let open = el.opening_tag_range.clone();
    if let Some(slice) = text.get(open.clone())
        && let Some(off) = slice.find(attr_name)
    {
        let start = open.start + off;
        return index.range(&(start..start + attr_name.len()));
    }
    if let Some(start) = text.find(attr_name) {
        return index.range(&(start..start + attr_name.len()));
    }
    index.range(&(0..0))
}

/// pocopine loop-scope magic variables (without the `$`). Mirrors the TS
/// server's `MAGIC_VARIABLES`.
const MAGIC_VARS: &[&str] = &[
    "index", "first", "last", "event", "el", "store", "route", "dispatch", "watch", "emit",
    "inject", "provide",
];

fn collect_loop_vars(ast: &pocopine_template_parser::TemplateAst, out: &mut HashSet<String>) {
    use pocopine_template_parser::Node;
    fn walk(el: &pocopine_template_parser::Element, out: &mut HashSet<String>) {
        for (name, value) in &el.attrs {
            if name == "pp-for" {
                // `item in items` → bind `item`.
                let mut parts = value.split_whitespace();
                if let (Some(var), Some("in")) = (parts.next(), parts.next())
                    && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !var.is_empty()
                {
                    out.insert(var.to_string());
                }
            }
        }
        for child in &el.children {
            if let Node::Element(ce) = child {
                walk(ce, out);
            }
        }
    }
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, out);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_symbols(
    ast: &pocopine_template_parser::TemplateAst,
    text: &str,
    index: &LineIndex,
    components: &ComponentIndex,
    own: Option<&component_index::ComponentDecl>,
    loop_vars: &HashSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    use pocopine_directives::parse_directive_attr;
    use pocopine_template_parser::Node;

    fn walk(
        el: &pocopine_template_parser::Element,
        text: &str,
        index: &LineIndex,
        components: &ComponentIndex,
        own: Option<&component_index::ComponentDecl>,
        loop_vars: &HashSet<String>,
        out: &mut Vec<Diagnostic>,
    ) {
        let is_component_tag = el.tag.contains('-');
        for (name, value) in &el.attrs {
            let Some(parsed) = parse_directive_attr(name) else {
                continue;
            };

            if let Some(comp) = own {
                check_unknown_ident(el, name, &parsed, value, comp, loop_vars, text, index, out);
            }
            if is_component_tag && let Some(child) = components.by_tag(&el.tag) {
                check_child_binding(el, name, &parsed, child, text, index, out);
            }
        }
        for child in &el.children {
            if let Node::Element(ce) = child {
                walk(ce, text, index, components, own, loop_vars, out);
            }
        }
    }

    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, text, index, components, own, loop_vars, out);
        }
    }
}

fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// `pp-on` / `@` values name a handler; `pp-text/html/show/if/bind/model` and
/// the `:`/`@` shorthands name a field. Anything else carries no checkable
/// single identifier.
fn directive_value_kind(head: &str) -> Option<&'static str> {
    match head {
        "on" => Some("handler"),
        "text" | "html" | "show" | "if" | "bind" | "model" => Some("field"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_unknown_ident(
    el: &pocopine_template_parser::Element,
    attr_name: &str,
    parsed: &pocopine_directives::ParsedAttr,
    value: &str,
    comp: &component_index::ComponentDecl,
    loop_vars: &HashSet<String>,
    text: &str,
    index: &LineIndex,
    out: &mut Vec<Diagnostic>,
) {
    let ident = value.trim();
    if !is_bare_ident(ident) {
        return; // not a single identifier — could be an expression/path
    }
    if matches!(ident, "true" | "false" | "null") {
        return;
    }
    if ident.starts_with('$') {
        return; // magic variable (e.g. $event, $index)
    }
    if let Some(stripped) = ident.strip_prefix('$')
        && MAGIC_VARS.contains(&stripped)
    {
        return;
    }
    if loop_vars.contains(ident) {
        return;
    }

    let range = attr_name_range(el, attr_name, text, index);
    match directive_value_kind(&parsed.head) {
        Some("handler") if !comp.has_handler(ident) => out.push(diag(
            range,
            DiagnosticSeverity::WARNING,
            format!("Unknown handler '{ident}' on component. Not found in impl block."),
        )),
        Some("field") if comp.field(ident).is_none() => out.push(diag(
            range,
            DiagnosticSeverity::WARNING,
            format!("Unknown identifier '{ident}'. Not a field on the component struct."),
        )),
        _ => {}
    }
}

fn check_child_binding(
    el: &pocopine_template_parser::Element,
    attr_name: &str,
    parsed: &pocopine_directives::ParsedAttr,
    child: &component_index::ComponentDecl,
    text: &str,
    index: &LineIndex,
    out: &mut Vec<Diagnostic>,
) {
    // Only `:prop` / `pp-bind:prop` and `pp-model:prop` write a child prop.
    // Native `pp-model` (no arg) binds the parent's input, not a child prop.
    let target = match parsed.head.as_str() {
        "bind" | "model" => parsed.arg.as_deref().filter(|a| !a.is_empty()),
        _ => None,
    };
    let Some(target) = target else {
        return;
    };

    let snake = target.replace('-', "_");
    let field = child.field(target).or_else(|| child.field(&snake));
    let range = attr_name_range(el, attr_name, text, index);

    let Some(field) = field else {
        out.push(diag(
            range,
            DiagnosticSeverity::WARNING,
            format!("<{}> has no field named '{target}'.", el.tag),
        ));
        return;
    };

    if field.role == FieldRole::State {
        let msg = if parsed.head == "model" {
            format!(
                "pp-model:{target} targets a state field on <{}>; parent writes are silently dropped. Annotate it as #[prop] or #[model] on the child.",
                el.tag
            )
        } else {
            format!(
                ":{target} / pp-bind:{target} writes to a state field on <{}>; the write is silently dropped at runtime.",
                el.tag
            )
        };
        out.push(diag(range, DiagnosticSeverity::WARNING, msg));
    }
}

// ─── document outline ──────────────────────────────────────────────────────

/// Build a [`DocumentSymbol`] tree from the template AST: one node per element,
/// nested by containment, named by tag with a directive summary as detail.
fn outline(ast: &pocopine_template_parser::TemplateAst, index: &LineIndex) -> Vec<DocumentSymbol> {
    ast.roots
        .iter()
        .filter_map(|n| n.as_element())
        .map(|el| element_symbol(el, index))
        .collect()
}

#[allow(deprecated)] // `DocumentSymbol::deprecated` is a required (deprecated) field
fn element_symbol(el: &pocopine_template_parser::Element, index: &LineIndex) -> DocumentSymbol {
    let is_component = el.tag.contains('-');
    let kind = if is_component {
        SymbolKind::CLASS
    } else if el.tag == "template" {
        SymbolKind::NAMESPACE
    } else {
        SymbolKind::FIELD
    };

    // Summarise the pocopine directives on this element as the symbol detail.
    let detail = {
        let dirs: Vec<&str> = el
            .attrs
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| n.starts_with("pp-") || n.starts_with(':') || n.starts_with('@'))
            .collect();
        if dirs.is_empty() {
            None
        } else {
            Some(dirs.join(" "))
        }
    };

    // Prefer the full element range; fall back to the opening tag when the
    // parser left `byte_range` as the `0..0` placeholder.
    let full = if el.byte_range.end > el.byte_range.start {
        el.byte_range.clone()
    } else {
        el.opening_tag_range.clone()
    };
    let range = index.range(&full);
    let selection = index.range(&el.opening_tag_range);

    let children: Vec<DocumentSymbol> = el
        .children
        .iter()
        .filter_map(|n| n.as_element())
        .map(|c| element_symbol(c, index))
        .collect();

    DocumentSymbol {
        name: el.tag.clone(),
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: selection,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    }
}

// ─── completion / hover / goto helpers ─────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum CompletionCtx {
    TagName,
    AttributeName,
    DirectiveArg(String),
    DirectiveModifier(String),
    TransitionPreset,
    FieldValue,
    HandlerValue,
    ClassValue,
    Interpolation,
    None,
}

/// Heuristic completion context from the text before the cursor — a port of the
/// TypeScript server's `detectContext`, trimmed to the contexts the Rust server
/// completes today.
fn detect_context(before: &str) -> CompletionCtx {
    // Interpolation: an unclosed `{{` after the last `}}`.
    if let Some(open) = before.rfind("{{")
        && before.rfind("}}").map(|close| open > close).unwrap_or(true)
    {
        return CompletionCtx::Interpolation;
    }

    // Inside a tag? Need a `<` not yet closed by `>`.
    let Some(last_lt) = before.rfind('<') else {
        return CompletionCtx::None;
    };
    if before.rfind('>').map(|gt| gt > last_lt).unwrap_or(false) {
        return CompletionCtx::None;
    }
    let tag_slice = &before[last_lt..];

    // Tag name: `<`, `<f`, `</fo` — no whitespace yet after the name.
    if !tag_slice.contains(char::is_whitespace) {
        return CompletionCtx::TagName;
    }

    // Inside an open attribute value?
    if let Some(attr) = current_value_attr(tag_slice) {
        if attr == "pp-transition" || attr == "pp-transition:in" || attr == "pp-transition:out" {
            return CompletionCtx::TransitionPreset;
        }
        if attr == "class" || attr == ":class" || attr == "pp-bind:class" {
            return CompletionCtx::ClassValue;
        }
        if attr.starts_with("pp-on:") || attr.starts_with('@') {
            return CompletionCtx::HandlerValue;
        }
        if attr == "pp-text"
            || attr == "pp-html"
            || attr == "pp-show"
            || attr == "pp-if"
            || attr.starts_with("pp-bind:")
            || attr.starts_with("pp-model")
            || (attr.starts_with(':') && attr.len() > 1)
        {
            return CompletionCtx::FieldValue;
        }
        return CompletionCtx::None; // some other attribute's value
    }

    // Typing an attribute name — `:arg` and `.modifier` get enum completion.
    let tail = tag_slice.rsplit(char::is_whitespace).next().unwrap_or("");
    if let Some(parsed) = pocopine_directives::parse_directive_attr(tail)
        && pocopine_directives::lookup(&parsed.head).is_some()
    {
        if tail.contains('.') {
            return CompletionCtx::DirectiveModifier(parsed.head);
        }
        if tail.contains(':') || tail.starts_with('@') {
            return CompletionCtx::DirectiveArg(parsed.head);
        }
    }
    CompletionCtx::AttributeName
}

/// If the cursor (end of `tag_slice`) sits inside an unclosed attribute value,
/// return that attribute's name.
fn current_value_attr(tag_slice: &str) -> Option<String> {
    let mut quote: Option<char> = None;
    let mut value_start = 0;
    for (i, c) in tag_slice.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                value_start = i;
            }
            None => {}
        }
    }
    quote?; // a quote is still open at the cursor
    let head = tag_slice[..value_start].trim_end();
    let head = head.strip_suffix('=')?.trim_end();
    let name: String = head
        .chars()
        .rev()
        .take_while(|c| !c.is_whitespace() && *c != '<')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The identifier-ish token at `col` (0-based char index) in `line`, plus its
/// start column. Token chars include the directive punctuation `: - @ . _ $`.
fn token_at(line: &str, col: usize) -> Option<(String, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let is_tok =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '@' | '.' | '$');
    let col = col.min(chars.len());
    let mut start = col;
    while start > 0 && is_tok(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_tok(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some((chars[start..end].iter().collect(), start))
}

fn role_str(role: FieldRole) -> &'static str {
    match role {
        FieldRole::Prop => "prop",
        FieldRole::Model => "model",
        FieldRole::State => "state",
    }
}

fn hover_md(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

fn component_contract_md(comp: &component_index::ComponentDecl) -> String {
    let mut md = format!("**<{}>** — `{}`", comp.tag, comp.name);
    if !comp.fields.is_empty() {
        md.push_str("\n\nFields:");
        for f in &comp.fields {
            md.push_str(&format!("\n- `{}` ({})", f.name, role_str(f.role)));
        }
    }
    if !comp.handlers.is_empty() {
        md.push_str("\n\nHandlers:");
        for h in &comp.handlers {
            md.push_str(&format!("\n- `{}`", h.name));
        }
    }
    md
}

fn location(file: &Path, pos: component_index::Pos) -> Option<Location> {
    let uri = Url::from_file_path(file).ok()?;
    let p = Position::new(pos.line, pos.character);
    Some(Location::new(uri, LspRange::new(p, p)))
}

fn tag_completions(index: &ComponentIndex) -> Vec<CompletionItem> {
    index
        .iter()
        .map(|c| CompletionItem {
            label: c.tag.clone(),
            kind: Some(CompletionItemKind::CLASS),
            detail: Some(format!("#[component] {}", c.name)),
            insert_text: Some(format!("{}>$0</{}>", c.tag, c.tag)),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: Some(format!("~{}", c.tag)),
            ..Default::default()
        })
        .collect()
}

fn directive_name_completions() -> Vec<CompletionItem> {
    use pocopine_directives::{ArgReq, DIRECTIVES};
    let mut items = Vec::new();
    for spec in DIRECTIVES {
        let insert = if spec.name == "route" || spec.name == "as" {
            format!("pp-{}", spec.name)
        } else if spec.takes_arg == ArgReq::Required {
            format!("pp-{}:${{1:arg}}=\"${{2:value}}\"", spec.name)
        } else {
            format!("pp-{}=\"${{1:value}}\"", spec.name)
        };
        items.push(CompletionItem {
            label: format!("pp-{}", spec.name),
            kind: Some(CompletionItemKind::PROPERTY),
            insert_text: Some(insert),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: spec.hover_doc.to_string(),
            })),
            detail: spec.rfc.map(|r| format!("({r})")),
            ..Default::default()
        });
        if let Some(sh) = spec.shorthand {
            let placeholder = if spec.name == "bind" { "attr" } else { "event" };
            items.push(CompletionItem {
                label: format!("{sh}{placeholder}"),
                kind: Some(CompletionItemKind::PROPERTY),
                insert_text: Some(format!("{sh}${{1:{placeholder}}}=\"${{2:value}}\"")),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }
    }
    items
}

fn own_fields(index: Option<&ComponentIndex>, own: Option<&str>) -> Vec<CompletionItem> {
    let Some(comp) = index.zip(own).and_then(|(i, n)| i.by_name(n)) else {
        return Vec::new();
    };
    comp.fields
        .iter()
        .map(|f| CompletionItem {
            label: f.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("{} field", role_str(f.role))),
            ..Default::default()
        })
        .collect()
}

fn own_handlers(index: Option<&ComponentIndex>, own: Option<&str>) -> Vec<CompletionItem> {
    let Some(comp) = index.zip(own).and_then(|(i, n)| i.by_name(n)) else {
        return Vec::new();
    };
    comp.handlers
        .iter()
        .map(|h| CompletionItem {
            label: h.name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some("component handler".to_string()),
            ..Default::default()
        })
        .collect()
}

fn directive_arg_completions(head: &str) -> Vec<CompletionItem> {
    let args = pocopine_directives::valid_args(head);
    if !args.is_empty() {
        return args
            .iter()
            .map(|a| CompletionItem {
                label: a.to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                ..Default::default()
            })
            .collect();
    }
    // Free-arg directives: only `pp-on`/`@` get a useful list (common events).
    if head == "on" {
        return common_events();
    }
    Vec::new()
}

fn directive_modifier_completions(head: &str) -> Vec<CompletionItem> {
    pocopine_directives::modifiers(head)
        .iter()
        .map(|m| CompletionItem {
            label: m.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect()
}

fn transition_preset_completions() -> Vec<CompletionItem> {
    pocopine_directives::TRANSITION_PRESETS
        .iter()
        .map(|p| CompletionItem {
            label: p.to_string(),
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            detail: Some("pp-transition preset (RFC-038)".to_string()),
            ..Default::default()
        })
        .collect()
}

fn common_events() -> Vec<CompletionItem> {
    [
        "click",
        "dblclick",
        "input",
        "change",
        "submit",
        "focus",
        "blur",
        "keydown",
        "keyup",
        "keypress",
        "mouseenter",
        "mouseleave",
        "mousedown",
        "mouseup",
        "mousemove",
        "wheel",
        "pointerdown",
        "pointerup",
        "pointermove",
    ]
    .iter()
    .map(|e| CompletionItem {
        label: e.to_string(),
        kind: Some(CompletionItemKind::EVENT),
        ..Default::default()
    })
    .collect()
}

fn magic_completions() -> Vec<CompletionItem> {
    MAGIC_VARS
        .iter()
        .map(|m| CompletionItem {
            label: format!("${m}"),
            kind: Some(CompletionItemKind::VARIABLE),
            detail: Some("pocopine magic variable".to_string()),
            ..Default::default()
        })
        .collect()
}

fn class_completions() -> Vec<CompletionItem> {
    use pocopine_stylekit::catalog::{ValueKind, catalog};
    let cat = catalog();
    let mut items = Vec::new();
    for group in &cat.groups {
        for f in &group.families {
            let valueless = matches!(f.value, ValueKind::None);
            items.push(CompletionItem {
                label: f.name.to_string(),
                kind: Some(if matches!(f.value, ValueKind::Color) {
                    CompletionItemKind::COLOR
                } else {
                    CompletionItemKind::VALUE
                }),
                detail: Some(format!("e.g. {}", f.example)),
                documentation: Some(Documentation::String(f.note.to_string())),
                insert_text: Some(if valueless {
                    f.name.to_string()
                } else {
                    format!("{}-", f.name)
                }),
                ..Default::default()
            });
        }
    }
    for v in &cat.variants {
        items.push(CompletionItem {
            label: format!("{}:", v.name),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(v.selector.to_string()),
            insert_text: Some(format!("{}:", v.name)),
            ..Default::default()
        });
    }
    items
}

// ─── rename (pp-for loop variables only) ───────────────────────────────────

/// The bare identifier (no `.`/`:`/`@`) at `col` in `line`, plus its start col.
/// Narrower than [`token_at`]: a path member like `item.foo` resolves to just
/// the segment under the cursor.
fn ident_at(line: &str, col: usize) -> Option<(String, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let is_id = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let col = col.min(chars.len());
    let mut start = col;
    while start > 0 && is_id(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_id(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some((chars[start..end].iter().collect(), start))
}

/// All ranges to rewrite when renaming loop variable `name`: head-position
/// occurrences (not member accesses, not inside string literals) within every
/// expression context — directive values and `{{ }}` interpolations. The
/// `pp-for` binding is itself an expression value, so it's covered uniformly.
fn loop_var_occurrences(
    ast: &pocopine_template_parser::TemplateAst,
    text: &str,
    index: &LineIndex,
    name: &str,
) -> Vec<LspRange> {
    let mut regions: Vec<(usize, String)> = Vec::new();
    collect_expr_regions(ast, text, &mut regions);
    let mut out = Vec::new();
    for (offset, expr) in &regions {
        for (s, e) in find_head_idents(expr, name) {
            out.push(index.range(&(offset + s..offset + e)));
        }
    }
    out
}

/// Whether a directive's value is an expression (vs. a static name like
/// `pp-ref`, `pp-route`, `pp-slot`, `pp-let`, `pp-as`).
fn is_expr_directive(head: &str) -> bool {
    !matches!(head, "ref" | "route" | "slot" | "let" | "as")
}

fn collect_expr_regions(
    ast: &pocopine_template_parser::TemplateAst,
    text: &str,
    out: &mut Vec<(usize, String)>,
) {
    use pocopine_template_parser::Node;
    fn walk(el: &pocopine_template_parser::Element, text: &str, out: &mut Vec<(usize, String)>) {
        for (attr_name, _) in &el.attrs {
            if let Some(parsed) = pocopine_directives::parse_directive_attr(attr_name)
                && is_expr_directive(&parsed.head)
                && let Some((s, e)) = attr_value_span(el, attr_name, text)
            {
                out.push((s, text[s..e].to_string()));
            }
        }
        for child in &el.children {
            if let Node::Element(ce) = child {
                walk(ce, text, out);
            }
        }
    }
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, text, out);
        }
    }
    collect_interp_regions(text, out);
}

fn collect_interp_regions(text: &str, out: &mut Vec<(usize, String)>) {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if i + 4 <= len && &bytes[i..i + 4] == b"<!--" {
            match find_seq(bytes, i + 4, b"-->") {
                Some(end) => {
                    i = end + 3;
                    continue;
                }
                None => break,
            }
        }
        if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'{' {
            if i > 0 && bytes[i - 1] == b'\\' {
                i += 2;
                continue;
            }
            let inner_start = i + 2;
            match find_seq(bytes, inner_start, b"}}") {
                Some(close) => {
                    out.push((inner_start, text[inner_start..close].to_string()));
                    i = close + 2;
                }
                None => break,
            }
        } else {
            i += 1;
        }
    }
}

/// Byte span (start..end) of an attribute's quoted value within `text`, found
/// inside the element's opening tag. `None` if the tag has no source span or
/// the value can't be located.
fn attr_value_span(
    el: &pocopine_template_parser::Element,
    attr_name: &str,
    text: &str,
) -> Option<(usize, usize)> {
    let open = el.opening_tag_range.clone();
    let slice = text.get(open.clone())?;
    let name_pos = slice.find(attr_name)?;
    let after = &slice[name_pos + attr_name.len()..];
    let eq = after.find('=')?;
    let rest = &after[eq + 1..];
    let q_rel = rest.find(['"', '\''])?;
    let quote = rest.as_bytes()[q_rel] as char;
    let close_rel = rest[q_rel + 1..].find(quote)?;
    let base = open.start + name_pos + attr_name.len() + eq + 1;
    let start = base + q_rel + 1;
    Some((start, start + close_rel))
}

/// Head-position occurrences of `name` in an expression string: a maximal
/// identifier token equal to `name`, not immediately preceded by `.` (so member
/// accesses like `other.name` don't match) and not inside a string literal.
fn find_head_idents(expr: &str, name: &str) -> Vec<(usize, usize)> {
    let bytes = expr.as_bytes();
    let is_start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b == b'$';
    let is_cont = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut out = Vec::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
            i += 1;
            continue;
        }
        if is_start(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_cont(bytes[i]) {
                i += 1;
            }
            let prev = if start > 0 { bytes[start - 1] } else { 0 };
            if prev != b'.' && &expr[start..i] == name {
                out.push((start, i));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// `{{expr}}` interpolation diagnostics via the shared expression parser
/// ([`pocopine_expr::parse`]) — the same parser the `#[component]` macro uses,
/// so a malformed interpolation flags here exactly as it fails to compile.
///
/// RFC-040: interpolation is double-brace `{{…}}`; a single `{` is literal
/// everywhere, and `\{{` escapes a literal `{{`. One scanner therefore covers
/// both text (`<p>{{name}}</p>`) and attribute (`class="x-{{v}}"`)
/// interpolation. HTML comments are skipped so `<!-- {{ x }} -->` never flags.
/// Bare directive-value expressions (`pp-text="count + 1"`) are NOT handled
/// here — those need the directive spec to know which attributes carry
/// expressions, and are the next layer.
fn interpolation_diagnostics(text: &str, index: &LineIndex) -> Vec<Diagnostic> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;

    while i < len {
        // Skip HTML comments wholesale.
        if i + 4 <= len && &bytes[i..i + 4] == b"<!--" {
            match find_seq(bytes, i + 4, b"-->") {
                Some(end) => {
                    i = end + 3;
                    continue;
                }
                None => break,
            }
        }

        // Interpolation opens on `{{`; a lone `{` is literal text.
        if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'{' {
            // `\{{` is an escaped literal — skip past the braces.
            if i > 0 && bytes[i - 1] == b'\\' {
                i += 2;
                continue;
            }
            let inner_start = i + 2;
            match find_seq(bytes, inner_start, b"}}") {
                Some(close) => {
                    let full = i..close + 2;
                    let inner = &text[inner_start..close];
                    if inner.trim().is_empty() {
                        out.push(diag(
                            index.range(&full),
                            DiagnosticSeverity::WARNING,
                            "empty `{{}}` interpolation — an expression is required".to_string(),
                        ));
                    } else if let Err(e) = pocopine_expr::parse(inner) {
                        // e.span is a byte range into `inner`; shift into doc space.
                        let start = inner_start + e.span.start.min(inner.len());
                        let end = inner_start + e.span.end.min(inner.len());
                        let end = end.max(start);
                        let message = match e.hint {
                            Some(h) => format!("{} — {}", e.message, h),
                            None => e.message,
                        };
                        out.push(diag(
                            index.range(&(start..end)),
                            DiagnosticSeverity::ERROR,
                            message,
                        ));
                    }
                    i = close + 2;
                }
                None => {
                    // No closing `}}` before EOF — flag the opener's line.
                    let line_end = text[i..].find('\n').map(|n| i + n).unwrap_or(len);
                    out.push(diag(
                        index.range(&(i..line_end)),
                        DiagnosticSeverity::WARNING,
                        "unterminated `{{` interpolation — add a closing `}}`".to_string(),
                    ));
                    break;
                }
            }
        } else {
            i += 1;
        }
    }

    out
}

/// First index `>= from` where `needle` begins in `haystack`, else `None`.
fn find_seq(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Byte-offset → LSP [`Position`] converter. LSP positions are zero-based
/// line + UTF-16 code-unit column, so we precompute line starts once and count
/// UTF-16 units from the line start to the offset.
struct LineIndex<'a> {
    src: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(src: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { src, line_starts }
    }

    fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.src.len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let line_start = self.line_starts[line];
        let character = self.src[line_start..offset].encode_utf16().count() as u32;
        Position::new(line as u32, character)
    }

    fn range(&self, range: &ByteRange<usize>) -> LspRange {
        LspRange::new(self.position(range.start), self.position(range.end))
    }

    /// LSP [`Position`] → byte offset (inverse of [`position`]).
    fn offset_at(&self, pos: Position) -> usize {
        let line = pos.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.src.len();
        };
        let line_text = self.line_text(pos.line);
        let mut utf16 = 0u32;
        for (byte_off, ch) in line_text.char_indices() {
            if utf16 >= pos.character {
                return line_start + byte_off;
            }
            utf16 += ch.len_utf16() as u32;
        }
        line_start + line_text.len()
    }

    /// The text of `line` (0-based), without the trailing newline.
    fn line_text(&self, line: u32) -> &str {
        let line = line as usize;
        let start = match self.line_starts.get(line) {
            Some(&s) => s,
            None => return "",
        };
        let end = self
            .line_starts
            .get(line + 1)
            .map(|&n| n.saturating_sub(1)) // drop the '\n'
            .unwrap_or(self.src.len());
        self.src.get(start..end).unwrap_or("")
    }
}
