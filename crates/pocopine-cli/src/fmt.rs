//! RFC-117 — `pocopine fmt`, structural rules for where a template lives.
//!
//! RFC-116 made `poco! { … }` and `Foo.poco` two spellings of one thing, with
//! size as the only real reason to prefer a file. Left to convention that
//! boundary drifts and every review re-argues it, so a rule owns it instead:
//! under the threshold a template is pulled inline, at or over it an inline
//! body is reported for extraction.
//!
//! Levels come from `[package.metadata.pocopine.fmt]` and behave like clippy:
//! `off` / `warn` / `fix`, with `--fix` promoting warnings for one run and
//! `--check` writing nothing and failing if anything would change.
//!
//! **v1 never edits template content.** Inlining is applied only to templates
//! that already lex as Rust tokens; anything else is reported and left alone.
//! Auto-escaping was specified in RFC-117 and deliberately dropped after
//! measuring: 35 of the repo's inline-eligible templates already contain HTML
//! entities, and quoting text that holds `&amp;` would re-escape the `&` into
//! a visible `&amp;amp;`. Silently corrupting content is the one thing a
//! formatter must never do, so the transform waits for a design that can
//! decode entities first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use syn::{Item, spanned::Spanned};

use crate::args::FmtArgs;
use crate::config::{FmtConfig, FmtLevel};

/// What a rule wants done to one component.
#[derive(Debug)]
enum Action {
    /// Replace the template argument with an inline body, then delete the file.
    /// The body already lives inside `edit.replacement`.
    Inline {
        template_file: PathBuf,
        /// Byte range in the `.rs` to overwrite, and what to put there.
        edit: Edit,
    },
    /// Write the inline body out to `path` and point the argument at it.
    Extract {
        path: PathBuf,
        body: String,
        edit: Edit,
    },
    /// Nothing to do, but the author should know why.
    Skipped { reason: String },
}

#[derive(Debug)]
struct Edit {
    range: std::ops::Range<usize>,
    replacement: String,
}

/// One rule firing against one component.
#[derive(Debug)]
struct Finding {
    file: PathBuf,
    component: String,
    rule: &'static str,
    lines: usize,
    action: Action,
}

pub fn run(args: &FmtArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = crate::config::load(&args.path)?;
    let fmt = cfg.fmt.unwrap_or_default();

    if fmt.inline_threshold == 0 {
        println!("▶ fmt: inline-threshold is 0 — both rules disabled");
        return Ok(());
    }

    let mut files = Vec::new();
    collect_rs(&project.join("src"), &mut files);
    files.sort();

    let mut findings = Vec::new();
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        findings.extend(scan_file(path, &source, &fmt));
    }

    resolve_conflicts(&mut findings);
    report_and_apply(&project, findings, &fmt, args)
}

/// Resolve every component in one file to a rule outcome.
fn scan_file(path: &Path, source: &str, cfg: &FmtConfig) -> Vec<Finding> {
    let Ok(parsed) = syn::parse_file(source) else {
        // Broken Rust is rustc's to report, not the formatter's.
        return Vec::new();
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    let mut structs = Vec::new();
    collect_structs(&parsed.items, &mut structs);

    for item_struct in structs {
        let Some(attr) = item_struct
            .attrs
            .iter()
            .find(|attr| attr.path().is_ident("component"))
        else {
            continue;
        };
        let name = item_struct.ident.to_string();
        let attr_range = attr.span().byte_range();

        match template_argument(attr, source) {
            TemplateArg::File { path: rel, range } => {
                let file = dir.join(&rel);
                let Ok(body) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let lines = body.lines().count();
                if lines >= cfg.inline_threshold || cfg.inline_small_templates == FmtLevel::Off {
                    continue;
                }
                let action = match unlexable_reason(&body) {
                    Some(reason) => Action::Skipped { reason },
                    None => Action::Inline {
                        template_file: file,
                        edit: inline_edit(source, attr_range.clone(), range, &body),
                    },
                };
                out.push(Finding {
                    file: path.to_path_buf(),
                    component: name,
                    rule: "inline_small_templates",
                    lines,
                    action,
                });
            }
            TemplateArg::Convention => {
                let file = dir.join(format!("{name}.poco"));
                let Ok(body) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let lines = body.lines().count();
                if lines >= cfg.inline_threshold || cfg.inline_small_templates == FmtLevel::Off {
                    continue;
                }
                let action = match unlexable_reason(&body) {
                    Some(reason) => Action::Skipped { reason },
                    None => Action::Inline {
                        template_file: file,
                        edit: inline_edit(source, attr_range.clone(), None, &body),
                    },
                };
                out.push(Finding {
                    file: path.to_path_buf(),
                    component: name,
                    rule: "inline_small_templates",
                    lines,
                    action,
                });
            }
            TemplateArg::Inline { body, range } => {
                let lines = body.lines().count();
                if lines < cfg.inline_threshold || cfg.extract_large_inline == FmtLevel::Off {
                    continue;
                }
                let target = format!("{name}.poco");
                out.push(Finding {
                    file: path.to_path_buf(),
                    component: name,
                    rule: "extract_large_inline",
                    lines,
                    action: Action::Extract {
                        path: dir.join(&target),
                        body: dedent(&body),
                        edit: Edit {
                            range,
                            replacement: format!("\"{target}\""),
                        },
                    },
                });
            }
            TemplateArg::None => {}
        }
    }
    out
}

/// Every struct in the file, including those nested in inline `mod` blocks —
/// a component declared inside one is still a component.
fn collect_structs<'a>(items: &'a [Item], out: &mut Vec<&'a syn::ItemStruct>) {
    for item in items {
        match item {
            Item::Struct(item_struct) => out.push(item_struct),
            Item::Mod(module) => {
                if let Some((_, inner)) = &module.content {
                    collect_structs(inner, out);
                }
            }
            _ => {}
        }
    }
}

enum TemplateArg {
    /// `template = "Foo.poco"` — the range covers the string literal.
    File {
        path: String,
        range: Option<std::ops::Range<usize>>,
    },
    /// `template = poco! { … }` — the range covers the whole macro call.
    Inline {
        body: String,
        range: std::ops::Range<usize>,
    },
    /// No `template` argument: the macro resolves `<Struct>.poco`.
    Convention,
    /// A bundle marker or similar — nothing to format.
    None,
}

fn template_argument(attr: &syn::Attribute, source: &str) -> TemplateArg {
    // A bare `#[component]` has no arguments to walk, and resolves by
    // convention.
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return TemplateArg::Convention;
    }

    let mut found: Option<TemplateArg> = None;
    let mut is_bundle = false;
    let _ = attr.parse_nested_meta(|meta| {
        let key = meta.path.get_ident().map(|ident| ident.to_string());
        if key.as_deref() == Some("extends") {
            is_bundle = true;
        }
        // Every value has to be consumed or the walk stops early.
        if let Ok(value) = meta.value() {
            let expr: syn::Expr = value.parse()?;
            if key.as_deref() == Some("template") {
                found = Some(match &expr {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(literal),
                        ..
                    }) => TemplateArg::File {
                        path: literal.value(),
                        range: Some(expr.span().byte_range()),
                    },
                    syn::Expr::Macro(macro_expr) => {
                        let range = expr.span().byte_range();
                        let inner = macro_expr.mac.tokens.span().byte_range();
                        let body = source
                            .get(inner)
                            .map(str::to_string)
                            .unwrap_or_else(|| macro_expr.mac.tokens.to_string());
                        TemplateArg::Inline { body, range }
                    }
                    _ => TemplateArg::None,
                });
            }
        }
        Ok(())
    });

    if is_bundle {
        return TemplateArg::None;
    }
    found.unwrap_or(TemplateArg::Convention)
}

/// The edit that turns a file template into an inline one.
///
/// With an explicit `template = "…"` the value is replaced in place. Without
/// one, the argument is inserted just after `component(` — ahead of any other
/// argument, which sidesteps trailing commas entirely.
fn inline_edit(
    source: &str,
    attr_range: std::ops::Range<usize>,
    value_range: Option<std::ops::Range<usize>>,
    body: &str,
) -> Edit {
    // Indent the template under the attribute rather than dumping it at
    // column zero. rustfmt will not touch a macro body, so whatever is
    // written here is what the author reads from then on.
    let indent = line_indent(source, attr_range.start);
    let inner = format!("{indent}    ");
    let indented = body
        .trim_matches('\n')
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{inner}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let macro_call = format!("poco! {{\n{indented}\n{indent}}}");

    if let Some(range) = value_range {
        return Edit {
            range,
            replacement: macro_call,
        };
    }

    let attr_text = &source[attr_range.clone()];
    match attr_text.find('(') {
        // `#[component(style = "x")]` → insert ahead of the existing args.
        Some(open) => {
            let at = attr_range.start + open + 1;
            Edit {
                range: at..at,
                replacement: format!("template = {macro_call}, "),
            }
        }
        // `#[component]` → give it an argument list.
        None => {
            let at = attr_range.start + attr_text.rfind(']').unwrap_or(attr_text.len() - 1);
            Edit {
                range: at..at,
                replacement: format!("(template = {macro_call})"),
            }
        }
    }
}

/// Downgrade any finding whose file would collide with another's.
///
/// Two components can legitimately point at one `.poco`, and two components in
/// different modules can share a struct name. Left alone, the first case
/// deletes the shared file once and then fails on the second delete — aborting
/// a run that has already written edits — and the second silently overwrites
/// one component's template with another's. Both are refused rather than
/// resolved: sharing a template is a structural choice the author made, and
/// duplicating it into two inline copies is not the formatter's call.
fn resolve_conflicts(findings: &mut [Finding]) {
    let mut inline_targets: std::collections::HashMap<PathBuf, usize> =
        std::collections::HashMap::new();
    let mut extract_targets: std::collections::HashMap<PathBuf, usize> =
        std::collections::HashMap::new();

    for finding in findings.iter() {
        match &finding.action {
            Action::Inline { template_file, .. } => {
                *inline_targets.entry(template_file.clone()).or_default() += 1;
            }
            Action::Extract { path, .. } => {
                *extract_targets.entry(path.clone()).or_default() += 1;
            }
            Action::Skipped { .. } => {}
        }
    }

    for finding in findings.iter_mut() {
        let reason = match &finding.action {
            Action::Inline { template_file, .. } if inline_targets[template_file] > 1 => {
                Some(format!(
                    "{} is shared by {} components — inlining would copy it into each",
                    display_name(template_file),
                    inline_targets[template_file]
                ))
            }
            Action::Extract { path, .. } if extract_targets[path] > 1 => Some(format!(
                "{} components would both extract to {}",
                extract_targets[path],
                display_name(path)
            )),
            // Never clobber a file that is already there; it is not this
            // component's template, since this component's template is inline.
            Action::Extract { path, .. } if path.exists() => Some(format!(
                "{} already exists — move or delete it first",
                display_name(path)
            )),
            _ => None,
        };
        if let Some(reason) = reason {
            finding.action = Action::Skipped { reason };
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The leading whitespace of the line `offset` sits on.
fn line_indent(source: &str, offset: usize) -> String {
    let line_start = source[..offset].rfind('\n').map(|nl| nl + 1).unwrap_or(0);
    source[line_start..offset]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect()
}

/// Remove the indentation an inline body carried from its attribute, so the
/// extracted file reads as though it had always been one. The inverse of the
/// indenting done above, which is what keeps a file → inline → file round
/// trip stable.
fn dedent(body: &str) -> String {
    let lines: Vec<&str> = body.trim_matches('\n').lines().collect();
    // The first line carries no indentation: a macro's token span starts at
    // the first token, not at the start of the line holding it. Including it
    // would make the common indent zero and dedent nothing.
    let common = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line.trim_start()
            } else if line.len() >= common {
                &line[common..]
            } else {
                line.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why a template cannot be inlined as-is, if it cannot.
///
/// Reuses the pre-lint's character rules, so "would `pocopine build` reject
/// this body?" has exactly one answer in the codebase.
fn unlexable_reason(body: &str) -> Option<String> {
    let offenders = crate::inline_lint::lexer_offenders(body);
    if offenders.is_empty() {
        return None;
    }
    let mut shown: Vec<String> = offenders.iter().take(3).map(|o| format!("`{o}`")).collect();
    if offenders.len() > 3 {
        shown.push(format!("and {} more", offenders.len() - 3));
    }
    Some(format!(
        "contains text the Rust lexer rejects ({})",
        shown.join(", ")
    ))
}

fn report_and_apply(
    project: &Path,
    findings: Vec<Finding>,
    cfg: &FmtConfig,
    args: &FmtArgs,
) -> Result<()> {
    if findings.is_empty() {
        println!("▶ fmt: nothing to do");
        return Ok(());
    }

    let mut edits: Vec<(PathBuf, Edit)> = Vec::new();
    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    let mut deletes: Vec<PathBuf> = Vec::new();
    let mut pending = 0usize;
    let mut skipped = 0usize;

    for finding in findings {
        let level = match finding.rule {
            "inline_small_templates" => cfg.inline_small_templates,
            _ => cfg.extract_large_inline,
        };
        // `--fix` promotes a warning to a rewrite for this run.
        let effective = if args.fix && level == FmtLevel::Warn {
            FmtLevel::Fix
        } else {
            level
        };
        let shown = finding
            .file
            .strip_prefix(project)
            .unwrap_or(&finding.file)
            .display()
            .to_string();

        match finding.action {
            Action::Skipped { reason } => {
                skipped += 1;
                println!(
                    "  skip  {} ({}, {} lines) — {reason}",
                    finding.component, shown, finding.lines
                );
            }
            Action::Inline {
                template_file,
                edit,
                ..
            } => {
                pending += 1;
                let verb = if effective == FmtLevel::Fix && !args.check {
                    edits.push((finding.file.clone(), edit));
                    deletes.push(template_file);
                    "inline"
                } else {
                    "would inline"
                };
                println!(
                    "  {verb}  {} ({}, {} lines)",
                    finding.component, shown, finding.lines
                );
            }
            Action::Extract {
                path, body, edit, ..
            } => {
                pending += 1;
                let verb = if effective == FmtLevel::Fix && !args.check {
                    edits.push((finding.file.clone(), edit));
                    writes.push((path, body));
                    "extract"
                } else {
                    "would extract"
                };
                println!(
                    "  {verb}  {} ({}, {} lines)",
                    finding.component, shown, finding.lines
                );
            }
        }
    }

    if args.check {
        if pending > 0 {
            bail!("{pending} template(s) are not in canonical form — run `pocopine fmt`");
        }
        println!("▶ fmt --check: {skipped} skipped, nothing to change");
        return Ok(());
    }

    apply_edits(edits)?;
    for (path, body) in writes {
        std::fs::write(&path, format!("{body}\n"))
            .with_context(|| format!("write {}", path.display()))?;
    }
    for path in deletes {
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

/// Apply every edit, grouped per file and back-to-front so earlier ranges
/// stay valid.
fn apply_edits(edits: Vec<(PathBuf, Edit)>) -> Result<()> {
    let mut by_file: std::collections::HashMap<PathBuf, Vec<Edit>> =
        std::collections::HashMap::new();
    for (path, edit) in edits {
        by_file.entry(path).or_default().push(edit);
    }
    for (path, mut file_edits) in by_file {
        let mut source =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        file_edits.sort_by_key(|edit| edit.range.start);
        for edit in file_edits.into_iter().rev() {
            source.replace_range(edit.range, &edit.replacement);
        }
        std::fs::write(&path, source).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                collect_rs(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> FmtConfig {
        FmtConfig::default()
    }

    fn findings(source: &str, dir: &Path) -> Vec<Finding> {
        scan_file(&dir.join("lib.rs"), source, &cfg())
    }

    /// A project directory holding one template file.
    fn scratch(template_name: &str, body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(template_name), body).expect("write template");
        dir
    }

    #[test]
    fn a_small_file_template_is_inlined_in_place() {
        let dir = scratch("Card.poco", "<div class=\"card\"></div>\n");
        let source = "#[component(template = \"Card.poco\", style = \"c.css\")]\nstruct Card;";
        let found = findings(source, dir.path());

        assert_eq!(found.len(), 1);
        let Action::Inline { edit, .. } = &found[0].action else {
            panic!("expected inline, got {:?}", found[0].action);
        };
        // Only the template value is rewritten; `style` is untouched.
        assert_eq!(&source[edit.range.clone()], "\"Card.poco\"");
        assert!(edit.replacement.starts_with("poco! {"));
        assert!(edit.replacement.contains("<div class=\"card\"></div>"));
    }

    #[test]
    fn a_convention_named_template_gains_an_argument() {
        let dir = scratch("Card.poco", "<div></div>\n");
        let source = "#[component(style = \"c.css\")]\nstruct Card;";
        let found = findings(source, dir.path());

        let Action::Inline { edit, .. } = &found[0].action else {
            panic!("expected inline");
        };
        // Inserted ahead of the existing argument, so no trailing-comma case.
        assert!(edit.range.is_empty(), "insertion, not replacement");
        assert!(edit.replacement.starts_with("template = poco! {"));
        assert!(edit.replacement.ends_with(", "));
    }

    #[test]
    fn a_bare_component_attribute_gains_an_argument_list() {
        let dir = scratch("Card.poco", "<div></div>\n");
        let source = "#[component]\nstruct Card;";
        let found = findings(source, dir.path());

        let Action::Inline { edit, .. } = &found[0].action else {
            panic!("expected inline");
        };
        assert!(edit.replacement.starts_with("(template = poco! {"));
        assert!(edit.replacement.ends_with(')'));
    }

    #[test]
    fn a_template_the_lexer_would_reject_is_skipped_not_mangled() {
        // The measured reason v1 does not transform content: rewriting this
        // safely means decoding entities first, and getting that wrong
        // corrupts the page silently.
        let dir = scratch("Card.poco", "<p>Don't stop — © 2026</p>\n");
        let source = "#[component(template = \"Card.poco\")]\nstruct Card;";
        let found = findings(source, dir.path());

        let Action::Skipped { reason } = &found[0].action else {
            panic!("expected skip, got {:?}", found[0].action);
        };
        assert!(reason.contains("Rust lexer rejects"), "{reason}");
    }

    #[test]
    fn a_template_at_the_threshold_is_left_as_a_file() {
        let body = "<div>\n".to_string() + &"  <p>x</p>\n".repeat(200) + "</div>\n";
        let dir = scratch("Card.poco", &body);
        let source = "#[component(template = \"Card.poco\")]\nstruct Card;";
        assert!(findings(source, dir.path()).is_empty());
    }

    #[test]
    fn a_large_inline_body_is_flagged_for_extraction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rows = "        <p>x</p>\n".repeat(200);
        let source = format!(
            "#[component(template = poco! {{\n    <div>\n{rows}    </div>\n}})]\nstruct Card;"
        );
        let found = findings(&source, dir.path());

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule, "extract_large_inline");
        let Action::Extract { path, edit, .. } = &found[0].action else {
            panic!("expected extract");
        };
        assert!(path.ends_with("Card.poco"));
        assert_eq!(edit.replacement, "\"Card.poco\"");
    }

    #[test]
    fn a_small_inline_body_is_already_canonical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = "#[component(template = poco! { <div>x</div> })]\nstruct Card;";
        assert!(findings(source, dir.path()).is_empty());
    }

    #[test]
    fn a_bundle_marker_owns_no_template() {
        let dir = scratch("Bundle.poco", "<div></div>\n");
        let source = "#[component(extends = [A, B])]\nstruct Bundle;";
        assert!(findings(source, dir.path()).is_empty());
    }

    #[test]
    fn a_missing_template_file_is_not_a_finding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = "#[component(template = \"Nope.poco\")]\nstruct Card;";
        assert!(findings(source, dir.path()).is_empty());
    }

    #[test]
    fn an_inlined_body_is_indented_under_its_attribute() {
        // rustfmt leaves macro bodies alone, so whatever is written here is
        // what the author reads from then on.
        let dir = scratch("Card.poco", "<div>\n  <p>x</p>\n</div>\n");
        let source = "mod inner {\n    #[component(template = \"Card.poco\")]\n    struct Card;\n}";
        let found = findings(source, dir.path());

        let Action::Inline { edit, .. } = &found[0].action else {
            panic!("expected inline");
        };
        // Attribute sits at four spaces, so the body sits at eight and the
        // closing brace lines up with the attribute.
        assert!(
            edit.replacement.contains("\n        <div>\n"),
            "{:?}",
            edit.replacement
        );
        assert!(edit.replacement.contains("\n          <p>x</p>\n"));
        assert!(edit.replacement.ends_with("\n    }"));
    }

    #[test]
    fn extraction_removes_the_indentation_inlining_added() {
        // A file → inline → file round trip has to land back where it
        // started, or the two rules fight each other forever.
        //
        // The shape matters: a macro's token span begins at the first token,
        // so the body's first line arrives with its indentation already gone
        // while every later line keeps it. Treating them alike leaves the
        // children hanging off a flush-left root.
        let original = "<div>\n  <p>x</p>\n</div>";
        let as_recovered = "<div>\n      <p>x</p>\n    </div>";
        assert_eq!(dedent(as_recovered), original);
    }

    #[test]
    fn a_template_shared_by_two_components_is_not_inlined() {
        // Applying this would copy one file into two inline bodies and then
        // fail on the second delete, part-way through a run that has already
        // written edits. Sharing is a structural choice; the formatter
        // reports it rather than undoing it.
        let dir = scratch("Shared.poco", "<div></div>\n");
        let source = "#[component(template = \"Shared.poco\")]\nstruct A;\n\
                      #[component(template = \"Shared.poco\")]\nstruct B;";
        let mut found = findings(source, dir.path());
        resolve_conflicts(&mut found);

        assert_eq!(found.len(), 2);
        for finding in &found {
            let Action::Skipped { reason } = &finding.action else {
                panic!("expected skip, got {:?}", finding.action);
            };
            assert!(reason.contains("shared by 2 components"), "{reason}");
        }
    }

    #[test]
    fn two_components_extracting_to_one_path_are_both_refused() {
        // Same struct name in two modules: without this, one body silently
        // overwrites the other and the loser's template is gone.
        let dir = tempfile::tempdir().expect("tempdir");
        let rows = "        <p>x</p>\n".repeat(200);
        let source = format!(
            "mod a {{ #[component(template = poco! {{\n{rows}}})]\nstruct Card; }}\n\
             mod b {{ #[component(template = poco! {{\n{rows}}})]\nstruct Card; }}"
        );
        let mut found = findings(&source, dir.path());
        resolve_conflicts(&mut found);

        assert_eq!(found.len(), 2);
        for finding in &found {
            let Action::Skipped { reason } = &finding.action else {
                panic!("expected skip, got {:?}", finding.action);
            };
            assert!(reason.contains("would both extract"), "{reason}");
        }
    }

    #[test]
    fn extraction_refuses_to_clobber_an_existing_file() {
        // The component's template is inline, so a file of that name belongs
        // to something else.
        let dir = scratch("Card.poco", "someone else's template\n");
        let rows = "        <p>x</p>\n".repeat(200);
        let source = format!("#[component(template = poco! {{\n{rows}}})]\nstruct Card;");
        let mut found = findings(&source, dir.path());
        resolve_conflicts(&mut found);

        let Action::Skipped { reason } = &found[0].action else {
            panic!("expected skip, got {:?}", found[0].action);
        };
        assert!(reason.contains("already exists"), "{reason}");
    }

    #[test]
    fn edits_apply_back_to_front_within_one_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lib.rs");
        std::fs::write(&path, "AAAA BBBB").expect("write");
        apply_edits(vec![
            (
                path.clone(),
                Edit {
                    range: 0..4,
                    replacement: "one".into(),
                },
            ),
            (
                path.clone(),
                Edit {
                    range: 5..9,
                    replacement: "two".into(),
                },
            ),
        ])
        .expect("apply");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one two");
    }
}
