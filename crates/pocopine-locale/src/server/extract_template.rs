use pocopine_expr::{Expr, Literal, Spanned};
use pocopine_template_parser::{Element, InterpolationSegment, Node};

use super::{Diagnostic, MessageReference, ReferenceKind, Span};
use crate::CatalogAudience;

#[derive(Clone, Debug)]
pub struct SourceContext {
    pub file: u32,
    /// Logical module namespace; platform `client`/`server` wrappers do not
    /// introduce a different namespace for the same application feature.
    /// An empty string denotes crate root, whose message namespace is `app`.
    pub module: String,
    pub audience: CatalogAudience,
    /// For an inline template, its starting byte in the original Rust file.
    pub offset: usize,
}

impl SourceContext {
    pub(crate) fn namespace(&self) -> &str {
        if self.module.is_empty() {
            "app"
        } else {
            &self.module
        }
    }
}

#[derive(Default, Debug)]
pub struct Extraction {
    pub references: Vec<MessageReference>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Extraction {
    pub fn append(&mut self, other: Self) {
        self.references.extend(other.references);
        self.diagnostics.extend(other.diagnostics);
    }
}

/// Discover static translation references through the same HTML and
/// expression grammar used by the component compiler. Comments, plain
/// attributes, and quoted expression strings are never references.
pub fn extract_template(source: &str, context: &SourceContext) -> Extraction {
    let mut out = Extraction::default();
    let (ast, errors) = pocopine_template_parser::parse(source, "locale template");
    for error in errors {
        // html5ever's duplicate-attribute observation is otherwise unspanned;
        // dropping an argument silently would invalidate the typed contract.
        if error.byte_range != (0..0)
            || error
                .message
                .to_ascii_lowercase()
                .contains("duplicate attribute")
        {
            out.diagnostics
                .push(Diagnostic::error(error.message).at(span(
                    context,
                    error.byte_range.start,
                    error.byte_range.end,
                )));
        }
    }
    for node in &ast.roots {
        walk(node, context, &mut out, span(context, 0, source.len()));
    }
    out
}

fn span(context: &SourceContext, start: usize, end: usize) -> Span {
    Span {
        file: context.file,
        start: (context.offset + start) as u32,
        end: (context.offset + end) as u32,
    }
}

fn walk(node: &Node, context: &SourceContext, out: &mut Extraction, parent_span: Span) {
    match node {
        Node::Element(element) => {
            let location = if element.synthetic {
                parent_span
            } else {
                span(
                    context,
                    element.opening_tag_range.start,
                    element.opening_tag_range.end,
                )
            };
            element_references(element, context, out, location);
            for child in &element.children {
                walk(child, context, out, location);
            }
        }
        Node::Text(text, _) => {
            if !text.contains("$t") {
                return;
            }
            match pocopine_template_parser::parse_interpolations(text) {
                Ok(segments) => {
                    for segment in segments {
                        if let InterpolationSegment::Dynamic(source) = segment {
                            expression(&source, context, out, parent_span);
                        }
                    }
                }
                Err(error) => out
                    .diagnostics
                    .push(Diagnostic::error(error).at(parent_span)),
            }
        }
        Node::Comment(_, _) => {}
    }
}

fn element_references(
    element: &Element,
    context: &SourceContext,
    out: &mut Extraction,
    location: Span,
) {
    for (name, source) in &element.attrs {
        if name == "pp-t"
            || name == ":pp-t"
            || name == "pp-bind:pp-t"
            || name.starts_with("pp-t:")
            || name.starts_with("pp-t@")
            || name.starts_with("pp-t.")
        {
            out.diagnostics.push(
                Diagnostic::error("pp-t is not supported; use $t in existing bindings")
                    .at(location),
            );
        } else if expression_attribute(name) && source.contains("$t") {
            match pocopine_expr::parse(source) {
                Ok(ast) => {
                    let direct = matches!(&ast.value, Expr::Path(parts) if parts.first().is_some_and(|p| p == "$t"))
                        || matches!(&ast.value, Expr::Call(name, _) if name == "$t");
                    let elements = if name == "pp-text" && direct {
                        if element.attrs.iter().any(|(name, _)| {
                            matches!(name.as_str(), "pp-html" | "pp-owned-content")
                        }) {
                            out.diagnostics.push(Diagnostic::error("translated pp-text cannot share content ownership with pp-html or pp-owned-content").at(location));
                        }
                        if element.children.iter().any(
                            |node| matches!(node, Node::Text(text, _) if !text.trim().is_empty()),
                        ) {
                            out.diagnostics.push(
                                Diagnostic::error(
                                    "translated pp-text copy belongs in the locale catalog",
                                )
                                .at(location),
                            );
                        }
                        Some(
                            element
                                .children
                                .iter()
                                .filter(|node| matches!(node, Node::Element(_)))
                                .count(),
                        )
                    } else {
                        None
                    };
                    expression_refs(&ast, context, out, location, elements);
                }
                Err(error) => out.diagnostics.push(
                    Diagnostic::error(format!("invalid translation expression: {}", error.message))
                        .at(location),
                ),
            }
        } else if name == "pp-for"
            && let Some((_, items)) = source.split_once(" in ")
        {
            expression(items, context, out, location);
        }
    }
}

fn expression_attribute(name: &str) -> bool {
    name.starts_with(':')
        || name.starts_with('@')
        || name.starts_with("pp-bind:")
        || name.starts_with("pp-on:")
        || name.starts_with("pp-model")
        || matches!(
            name,
            "pp-text" | "pp-html" | "pp-show" | "pp-if" | "pp-else-if" | "pp-match" | "pp-key"
        )
}

fn expression(source: &str, context: &SourceContext, out: &mut Extraction, location: Span) {
    if !source.contains("$t") {
        return;
    }
    match pocopine_expr::parse(source) {
        Ok(ast) => expression_refs(&ast, context, out, location, None),
        Err(error) => out.diagnostics.push(
            Diagnostic::error(format!("invalid translation expression: {}", error.message))
                .at(location),
        ),
    }
}

fn expression_refs(
    ast: &Spanned<Expr>,
    context: &SourceContext,
    out: &mut Extraction,
    location: Span,
    elements: Option<usize>,
) {
    match &ast.value {
        Expr::Path(parts) if parts.first().is_some_and(|part| part == "$t") => {
            if parts.len() < 3 {
                out.diagnostics.push(
                    Diagnostic::error("$t requires a complete static dotted message key")
                        .at(location),
                );
            } else {
                out.references.push(MessageReference {
                    key: parts[1..].join("."),
                    module: context.namespace().to_owned(),
                    kind: ReferenceKind::Template {
                        arguments: 0,
                        elements,
                    },
                    audience: context.audience,
                    span: location,
                });
            }
        }
        Expr::Not(value) => expression_refs(value, context, out, location, None),
        Expr::BinOp(_, left, right) => {
            expression_refs(left, context, out, location, None);
            expression_refs(right, context, out, location, None);
        }
        Expr::Ternary(condition, left, right) => {
            expression_refs(condition, context, out, location, None);
            expression_refs(left, context, out, location, None);
            expression_refs(right, context, out, location, None);
        }
        Expr::Assign(parts, value) => {
            if parts.first().is_some_and(|part| part == "$t") {
                out.diagnostics
                    .push(Diagnostic::error("$t translation paths are read-only").at(location));
            }
            expression_refs(value, context, out, location, None);
        }
        Expr::Call(name, args) => {
            if name == "$t" {
                match args.first().map(|arg| &arg.value) {
                    Some(Expr::Literal(Literal::String(key))) if key.contains('.') => {
                        out.references.push(MessageReference {
                            key: key.clone(),
                            module: context.namespace().to_owned(),
                            kind: ReferenceKind::Template {
                                arguments: args.len() - 1,
                                elements,
                            },
                            audience: context.audience,
                            span: location,
                        })
                    }
                    _ => out.diagnostics.push(
                        Diagnostic::error(
                            "$t requires a literal dotted message key as its first argument",
                        )
                        .at(location),
                    ),
                }
            }
            for arg in args {
                expression_refs(arg, context, out, location, None);
            }
        }
        Expr::Seq(values) => {
            for value in values {
                expression_refs(value, context, out, location, None);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> SourceContext {
        SourceContext {
            file: 2,
            module: "cart".into(),
            audience: CatalogAudience::Browser,
            offset: 30,
        }
    }

    #[test]
    fn reads_real_directives_and_paths_but_not_comments_or_literal_strings() {
        let source = r#"<div><!-- <p pp-text="$t.cart.fake"></p> -->
            <p pp-text="$t('cart.items', items.len)"></p>
            <input :placeholder="$t.cart.search" title="$t.cart.literal">
            <span pp-text="'$t.cart.quoted'"></span>
            <span>{{ ready ? $t.common.ready : $t.common.wait }}</span>
            <span>\{{ $t.cart.escaped }}</span></div>"#;
        let found = extract_template(source, &context());
        assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
        assert_eq!(
            found
                .references
                .iter()
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["cart.items", "cart.search", "common.ready", "common.wait"]
        );
        assert!(
            found
                .references
                .iter()
                .all(|r| r.span.file == 2 && r.span.start >= 30)
        );
        assert!(matches!(
            &found.references[0].kind,
            ReferenceKind::Template {
                arguments: 1,
                elements: Some(0)
            }
        ));
    }

    #[test]
    fn rejects_dynamic_keys_detached_args_mutation_and_competing_owners() {
        for source in [
            r#"<p :pp-t="key"></p>"#,
            r#"<p pp-t:count="n"></p>"#,
            r#"<p pp-t="cart.x" pp-text="x"></p>"#,
            r#"<p pp-text="$t.cart.x = 'oops'"></p>"#,
            r#"<p :title="$t(key)"></p>"#,
            r#"<p pp-text="$t.cart.x" pp-html="html"></p>"#,
            r#"<p pp-text="$t.cart.x">Baked copy</p>"#,
            r#"<p pp-t="cart.x">Baked copy</p>"#,
            r#"<p pp-t="cart.x" pp-t="cart.y"></p>"#,
        ] {
            let result = extract_template(source, &context());
            assert!(!result.diagnostics.is_empty(), "accepted {source}");
        }
    }
}
