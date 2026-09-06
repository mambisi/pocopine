use std::collections::BTreeMap;

use proc_macro2::{TokenStream, TokenTree};
use syn::{
    Meta, Token, ext::IdentExt, parse::Parser, punctuated::Punctuated, spanned::Spanned,
    visit::Visit,
};

use super::{
    CfgSet, Diagnostic, Extraction, MessageReference, ReferenceKind, SourceContext, Span,
    extract_template,
};
use crate::CatalogAudience;

/// A file reached from an active Rust module declaration. `parents` contains
/// inline Rust modules, including platform wrappers, for filesystem resolution.
#[derive(Debug)]
pub struct ModuleRequest {
    pub name: String,
    pub path: Option<String>,
    pub parents: Vec<String>,
    /// An outer inline #[path] starts at the source file directory.
    pub directory_from_file: bool,
    pub context: SourceContext,
    pub span: Span,
}

#[derive(Debug)]
pub struct TemplateRequest {
    pub path: String,
    pub context: SourceContext,
    pub span: Span,
}

#[derive(Debug, Default)]
pub struct RustExtraction {
    pub includes: Vec<IncludeRequest>,
    pub extracted: Extraction,
    pub modules: Vec<ModuleRequest>,
    pub templates: Vec<TemplateRequest>,
}

/// A literal include! shares the current logical module and module directory.
#[derive(Debug)]
pub struct IncludeRequest {
    pub path: String,
    pub parents: Vec<String>,
    /// An outer inline #[path] starts at the source file directory.
    pub directory_from_file: bool,
    pub context: SourceContext,
    pub span: Span,
}

/// Extract active Rust references and component templates without executing
/// source code or expanding arbitrary user macros. Rust cfg is evaluated using
/// the supplied compilation target, including its resolved Cargo features.
pub fn extract_rust(source: &str, context: &SourceContext, cfg: &CfgSet) -> RustExtraction {
    let mut walker = Walker {
        source,
        cfg,
        context: context.clone(),
        parents: Vec::new(),
        directory_from_file: false,
        aliases: BTreeMap::new(),
        out: RustExtraction::default(),
    };
    match syn::parse_file(source) {
        Ok(file) => {
            if walker.active(&file.attrs).is_some() {
                walker.items(&file.items);
            }
        }
        Err(error) => walker.error(
            format!("cannot extract locale references: {error}"),
            error.span(),
        ),
    }
    walker.out
}

struct Walker<'a> {
    source: &'a str,
    cfg: &'a CfgSet,
    context: SourceContext,
    parents: Vec<String>,
    directory_from_file: bool,
    aliases: BTreeMap<String, Vec<String>>,
    out: RustExtraction,
}

impl Walker<'_> {
    fn span(&self, span: proc_macro2::Span) -> Span {
        let range = span.byte_range();
        Span {
            file: self.context.file,
            start: (self.context.offset + range.start) as u32,
            end: (self.context.offset + range.end) as u32,
        }
    }

    fn error(&mut self, message: impl Into<String>, span: proc_macro2::Span) {
        self.out
            .extracted
            .diagnostics
            .push(Diagnostic::error(message).at(self.span(span)));
    }

    fn active(&mut self, attrs: &[syn::Attribute]) -> Option<Vec<Meta>> {
        match self.cfg.attributes(attrs) {
            Ok(attrs) => attrs,
            Err(error) => {
                self.error(
                    error,
                    attrs
                        .first()
                        .map(Spanned::span)
                        .unwrap_or_else(proc_macro2::Span::call_site),
                );
                None
            }
        }
    }

    fn reference(&mut self, parts: &[String], span: proc_macro2::Span) {
        if runtime_api(parts) {
            return;
        }
        if parts.len() < 2 {
            self.error(
                "Rust translation references require a complete t::module::message path",
                span,
            );
            return;
        }
        self.out.extracted.references.push(MessageReference {
            key: parts.join("."),
            module: self.context.namespace().to_owned(),
            kind: ReferenceKind::Rust,
            audience: self.context.audience,
            span: self.span(span),
        });
    }

    fn translation_path(&self, parts: &[String]) -> Option<Vec<String>> {
        let first = parts.first()?;
        if let Some(alias) = self.aliases.get(first) {
            let mut resolved = alias.clone();
            resolved.extend_from_slice(&parts[1..]);
            return Some(resolved);
        }
        let skip = parts
            .iter()
            .take_while(|part| matches!(part.as_str(), "crate" | "self" | "super"))
            .count();
        (parts.get(skip).is_some_and(|part| part == "t")).then(|| parts[skip + 1..].to_vec())
    }

    fn import(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>, emit: bool) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.unraw().to_string());
                self.import(&path.tree, prefix, emit);
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.import(tree, prefix, emit);
                }
            }
            syn::UseTree::Name(name) => {
                let mut path = prefix.clone();
                let alias = if name.ident == "self" {
                    prefix.last().cloned().unwrap_or_default()
                } else {
                    path.push(name.ident.unraw().to_string());
                    name.ident.unraw().to_string()
                };
                if let Some(resolved) = self.translation_path(&path) {
                    if emit {
                        self.import_reference(&resolved, name.span());
                    }
                    self.aliases.insert(alias, resolved);
                }
            }
            syn::UseTree::Rename(rename) => {
                let mut path = prefix.clone();
                if rename.ident != "self" {
                    path.push(rename.ident.unraw().to_string());
                }
                if let Some(resolved) = self.translation_path(&path) {
                    if emit {
                        self.import_reference(&resolved, rename.span());
                    }
                    self.aliases
                        .insert(rename.rename.unraw().to_string(), resolved);
                }
            }
            syn::UseTree::Glob(glob) => {
                if self.translation_path(prefix).is_some() {
                    self.error("translation imports must name the namespace or function; glob imports cannot be extracted statically", glob.span());
                }
            }
        }
    }

    fn import_reference(&mut self, parts: &[String], span: proc_macro2::Span) {
        if runtime_api(parts) {
            return;
        }
        self.out.extracted.references.push(MessageReference {
            key: parts.join("."),
            module: self.context.namespace().to_owned(),
            kind: ReferenceKind::RustImport,
            audience: self.context.audience,
            span: self.span(span),
        });
    }

    fn imports(&mut self, items: &[syn::Item]) {
        // Rust imports apply to the entire lexical scope, including preceding
        // functions. Repeat to resolve aliases declared in either source order.
        for _ in 0..=items.len() {
            let before = self.aliases.clone();
            for item in items {
                if let syn::Item::Use(item) = item
                    && self.cfg.attributes(&item.attrs).is_ok_and(|a| a.is_some())
                {
                    self.import(&item.tree, &mut Vec::new(), false);
                }
            }
            if before == self.aliases {
                break;
            }
        }
    }

    fn items(&mut self, items: &[syn::Item]) {
        self.imports(items);
        for item in items {
            self.visit_item(item);
        }
    }

    fn component(&mut self, name: &syn::Ident, meta: &Meta) {
        if matches!(meta, Meta::Path(_)) {
            self.out.templates.push(TemplateRequest {
                path: format!("{name}.poco"),
                context: self.context.clone(),
                span: self.span(name.span()),
            });
            return;
        }
        let Meta::List(list) = meta else {
            return;
        };
        let args = match Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())
        {
            Ok(args) => args,
            Err(error) => {
                self.error(
                    format!("cannot inspect component arguments: {error}"),
                    error.span(),
                );
                return;
            }
        };
        let template = args.iter().find(|arg| arg.path().is_ident("template"));
        if let Some(Meta::NameValue(pair)) = template {
            match &pair.value {
                syn::Expr::Lit(syn::ExprLit {lit: syn::Lit::Str(path), ..}) => {
                    self.out.templates.push(TemplateRequest {path: path.value(), context: self.context.clone(), span: self.span(path.span())});
                }
                syn::Expr::Macro(expr) if last_is(&expr.mac.path, "poco") => self.inline(&expr.mac),
                _ => self.error("component locale extraction requires a literal template path or inline poco! body", pair.span()),
            }
        } else if template.is_some() {
            self.error("component template must use template = ...", meta.span());
        } else {
            self.out.templates.push(TemplateRequest {
                path: format!("{name}.poco"),
                context: self.context.clone(),
                span: self.span(name.span()),
            });
        }
        for arg in args {
            if !arg.path().is_ident("template") {
                self.meta(&arg);
            }
        }
    }

    fn inline(&mut self, mac: &syn::Macro) {
        let range = match &mac.delimiter {
            syn::MacroDelimiter::Paren(delim) => delim.span.join().byte_range(),
            syn::MacroDelimiter::Brace(delim) => delim.span.join().byte_range(),
            syn::MacroDelimiter::Bracket(delim) => delim.span.join().byte_range(),
        };
        let Some(body) = range
            .start
            .checked_add(1)
            .zip(range.end.checked_sub(1))
            .and_then(|(start, end)| self.source.get(start..end).map(|body| (start, body)))
        else {
            self.error("inline template source span is unavailable", mac.span());
            return;
        };
        let normalized = pocopine_template_parser::inline_source::normalize_inline_text(
            body.1,
            mac.tokens.clone(),
            body.0,
        );
        let mut context = self.context.clone();
        context.offset = 0;
        let mut extracted = extract_template(&normalized.source, &context);
        let remap = |span: &mut Span| {
            span.start = (self.context.offset
                + body.0
                + normalized.original_offset(span.start as usize)) as u32;
            span.end = (self.context.offset
                + body.0
                + normalized.original_offset(span.end as usize)) as u32;
        };
        for reference in &mut extracted.references {
            remap(&mut reference.span);
        }
        for diagnostic in &mut extracted.diagnostics {
            remap(&mut diagnostic.span);
        }
        self.out.extracted.append(extracted);
    }

    fn meta(&mut self, meta: &Meta) {
        match meta {
            Meta::NameValue(pair) => self.visit_expr(&pair.value),
            Meta::List(list) if !list.path.is_ident("cfg") && !list.path.is_ident("cfg_attr") => {
                self.tokens(list.tokens.clone())
            }
            _ => {}
        }
    }

    fn tokens(&mut self, stream: TokenStream) {
        let tokens: Vec<_> = stream.into_iter().collect();
        let mut i = 0;
        while i < tokens.len() {
            if let TokenTree::Ident(first) = &tokens[i] {
                let mut parts = vec![first.unraw().to_string()];
                let mut end = i + 1;
                while matches!(tokens.get(end),Some(TokenTree::Punct(p)) if p.as_char()==':')
                    && matches!(tokens.get(end+1),Some(TokenTree::Punct(p)) if p.as_char()==':')
                    && let Some(TokenTree::Ident(next)) = tokens.get(end + 2)
                {
                    parts.push(next.unraw().to_string());
                    end += 3;
                }
                if parts
                    .last()
                    .is_some_and(|part| matches!(part.as_str(), "poco" | "include"))
                    && matches!(tokens.get(end), Some(TokenTree::Punct(p)) if p.as_char() == '!')
                    && matches!(tokens.get(end + 1), Some(TokenTree::Group(_)))
                {
                    let source = tokens[i..end + 2].iter().cloned().collect();
                    if let Ok(mac) = syn::parse2::<syn::Macro>(source) {
                        self.visit_macro(&mac);
                    }
                    i = end + 2;
                    continue;
                }
                if let Some(path) = self.translation_path(&parts) {
                    self.reference(&path, first.span());
                }
                i = end;
                continue;
            }
            if let TokenTree::Group(group) = &tokens[i] {
                self.tokens(group.stream());
            }
            i += 1;
        }
    }
}

fn last_is(path: &syn::Path, name: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|part| part.ident.unraw() == name)
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

impl<'ast> Visit<'ast> for Walker<'_> {
    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}

    fn visit_item(&mut self, item: &'ast syn::Item) {
        let Some(attrs) = self.active(item_attrs(item)) else {
            return;
        };
        let server = attrs.iter().any(|attr| last_is(attr.path(), "server"));
        if server && self.context.audience == CatalogAudience::Browser {
            return;
        }
        for attr in &attrs {
            if let syn::Item::Struct(item) = item
                && last_is(attr.path(), "component")
            {
                self.component(&item.ident, attr);
            } else {
                self.meta(attr);
            }
        }
        match item {
            syn::Item::Mod(item) => {
                // Modules do not inherit `use` aliases from their parent.
                let aliases = std::mem::take(&mut self.aliases);
                let previous = self.context.module.clone();
                let name = item.ident.unraw().to_string();
                self.context.module = module_namespace(&previous, &name);
                let path = attrs.iter().find_map(|attr| match attr {
                    Meta::NameValue(pair) if pair.path.is_ident("path") => match &pair.value {
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(path),
                            ..
                        }) => Some(path.value()),
                        _ => None,
                    },
                    _ => None,
                });
                if let Some((_, items)) = &item.content {
                    let previous_directory = self.directory_from_file;
                    if self.parents.is_empty() && path.is_some() {
                        self.directory_from_file = true;
                    }
                    self.parents.push(path.unwrap_or(name));
                    self.items(items);
                    self.parents.pop();
                    self.directory_from_file = previous_directory;
                } else {
                    self.out.modules.push(ModuleRequest {
                        name,
                        path,
                        parents: self.parents.clone(),
                        directory_from_file: self.directory_from_file,
                        context: self.context.clone(),
                        span: self.span(item.ident.span()),
                    });
                }
                self.context.module = previous;
                self.aliases = aliases;
            }
            syn::Item::Use(item) => self.import(&item.tree, &mut Vec::new(), true),
            _ => syn::visit::visit_item(self, item),
        }
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        let previous = self.aliases.clone();
        let items = block
            .stmts
            .iter()
            .filter_map(|s| {
                if let syn::Stmt::Item(i) = s {
                    Some(i.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        self.imports(&items);
        syn::visit::visit_block(self, block);
        self.aliases = previous;
    }

    fn visit_stmt_macro(&mut self, item: &'ast syn::StmtMacro) {
        if self.active(&item.attrs).is_some() {
            syn::visit::visit_stmt_macro(self, item);
        }
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if let Some(attrs) = self.active(&field.attrs) {
            for attr in &attrs {
                self.meta(attr);
            }
            syn::visit::visit_field(self, field);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs = match item {
            syn::ImplItem::Const(i) => &i.attrs,
            syn::ImplItem::Fn(i) => &i.attrs,
            syn::ImplItem::Type(i) => &i.attrs,
            syn::ImplItem::Macro(i) => &i.attrs,
            _ => {
                syn::visit::visit_impl_item(self, item);
                return;
            }
        };
        if self.active(attrs).is_some() {
            syn::visit::visit_impl_item(self, item);
        }
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs = match item {
            syn::TraitItem::Const(i) => &i.attrs,
            syn::TraitItem::Fn(i) => &i.attrs,
            syn::TraitItem::Type(i) => &i.attrs,
            syn::TraitItem::Macro(i) => &i.attrs,
            _ => {
                syn::visit::visit_trait_item(self, item);
                return;
            }
        };
        if self.active(attrs).is_some() {
            syn::visit::visit_trait_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let Some(attrs) = self.active(&item.attrs) else {
            return;
        };
        if self.context.audience == CatalogAudience::Browser
            && attrs.iter().any(|a| last_is(a.path(), "server"))
        {
            return;
        }
        for attr in &attrs {
            self.meta(attr);
        }
        syn::visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if self.active(&item.attrs).is_some() {
            syn::visit::visit_trait_item_fn(self, item);
        }
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if self.active(&local.attrs).is_some() {
            syn::visit::visit_local(self, local);
        }
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if self.active(&path.attrs).is_none() {
            return;
        }
        if path.qself.is_none() {
            let parts = path
                .path
                .segments
                .iter()
                .map(|part| part.ident.unraw().to_string())
                .collect::<Vec<_>>();
            if let Some(key) = self.translation_path(&parts) {
                self.reference(&key, path.span());
            }
        }
        syn::visit::visit_expr_path(self, path);
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        // Attributes can disable a whole expression subtree, not just paths.
        let attrs = match expr {
            syn::Expr::Array(e) => &e.attrs,
            syn::Expr::Assign(e) => &e.attrs,
            syn::Expr::Async(e) => &e.attrs,
            syn::Expr::Await(e) => &e.attrs,
            syn::Expr::Binary(e) => &e.attrs,
            syn::Expr::Block(e) => &e.attrs,
            syn::Expr::Break(e) => &e.attrs,
            syn::Expr::Call(e) => &e.attrs,
            syn::Expr::Cast(e) => &e.attrs,
            syn::Expr::Closure(e) => &e.attrs,
            syn::Expr::Const(e) => &e.attrs,
            syn::Expr::Continue(e) => &e.attrs,
            syn::Expr::Field(e) => &e.attrs,
            syn::Expr::ForLoop(e) => &e.attrs,
            syn::Expr::Group(e) => &e.attrs,
            syn::Expr::If(e) => &e.attrs,
            syn::Expr::Index(e) => &e.attrs,
            syn::Expr::Infer(e) => &e.attrs,
            syn::Expr::Let(e) => &e.attrs,
            syn::Expr::Lit(e) => &e.attrs,
            syn::Expr::Loop(e) => &e.attrs,
            syn::Expr::Macro(e) => &e.attrs,
            syn::Expr::Match(e) => &e.attrs,
            syn::Expr::MethodCall(e) => &e.attrs,
            syn::Expr::Paren(e) => &e.attrs,
            syn::Expr::Path(e) => &e.attrs,
            syn::Expr::Range(e) => &e.attrs,
            syn::Expr::RawAddr(e) => &e.attrs,
            syn::Expr::Reference(e) => &e.attrs,
            syn::Expr::Repeat(e) => &e.attrs,
            syn::Expr::Return(e) => &e.attrs,
            syn::Expr::Struct(e) => &e.attrs,
            syn::Expr::Try(e) => &e.attrs,
            syn::Expr::TryBlock(e) => &e.attrs,
            syn::Expr::Tuple(e) => &e.attrs,
            syn::Expr::Unary(e) => &e.attrs,
            syn::Expr::Unsafe(e) => &e.attrs,
            syn::Expr::While(e) => &e.attrs,
            syn::Expr::Yield(e) => &e.attrs,
            _ => {
                syn::visit::visit_expr(self, expr);
                return;
            }
        };
        if self.active(attrs).is_some() {
            syn::visit::visit_expr(self, expr);
        }
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if self.active(&arm.attrs).is_some() {
            syn::visit::visit_arm(self, arm);
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if last_is(&mac.path, "poco") {
            self.inline(mac);
        } else if last_is(&mac.path, "include") {
            if let Ok(path) = syn::parse2::<syn::LitStr>(mac.tokens.clone()) {
                self.out.includes.push(IncludeRequest {
                    path: path.value(),
                    parents: self.parents.clone(),
                    directory_from_file: self.directory_from_file,
                    context: self.context.clone(),
                    span: self.span(path.span()),
                });
            } else if !generated_locale_include(mac) {
                self.error("locale discovery requires a literal include! path; generated source must be supplied as an explicit source root",mac.span());
            }
        } else {
            let before = self.out.extracted.references.len();
            let includes_before = self.out.includes.len();
            self.tokens(mac.tokens.clone());
            if contains_component_attribute(mac.tokens.clone()) {
                self.error("components inside an opaque macro cannot be discovered statically; declare #[component] items outside the macro", mac.span());
            }
            if (self.out.extracted.references.len() > before
                || self.out.includes.len() > includes_before)
                && conditional_tokens(mac.tokens.clone())
            {
                self.error("translation references inside a conditional macro cannot be assigned to a target; use ordinary #[cfg] items outside the macro",mac.span());
            }
        }
    }
}

fn contains_component_attribute(stream: TokenStream) -> bool {
    let tokens = stream.into_iter().collect::<Vec<_>>();
    tokens.iter().enumerate().any(|(index, token)| {
        if matches!(token,TokenTree::Punct(p) if p.as_char()=='#')
            && let Some(TokenTree::Group(group)) = tokens.get(index + 1)
            && let Ok(meta) = syn::parse2::<Meta>(group.stream())
            && last_is(meta.path(), "component")
        {
            return true;
        }
        matches!(token,TokenTree::Group(group) if contains_component_attribute(group.stream()))
    })
}

fn generated_locale_include(mac: &syn::Macro) -> bool {
    // This one file is produced by locale codegen itself and has no authored
    // references. Other generated Rust needs explicit discovery input.
    let Ok(expr) = syn::parse2::<syn::ExprMacro>(mac.tokens.clone()) else {
        return false;
    };
    if !expr.mac.path.is_ident("concat") {
        return false;
    }
    let Ok(args) = Punctuated::<syn::Expr, Token![,]>::parse_terminated.parse2(expr.mac.tokens)
    else {
        return false;
    };
    if args.len() != 2 {
        return false;
    }
    matches!((&args[0],&args[1]),
        (syn::Expr::Macro(env),syn::Expr::Lit(syn::ExprLit {lit:syn::Lit::Str(file),..}))
        if env.mac.path.is_ident("env")
            && syn::parse2::<syn::LitStr>(env.mac.tokens.clone()).is_ok_and(|name|name.value()=="OUT_DIR")
            && file.value()=="/pocopine_locale.rs")
}

fn conditional_tokens(stream: TokenStream) -> bool {
    stream.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => {
            matches!(
                ident.unraw().to_string().as_str(),
                "cfg" | "cfg_attr" | "server"
            )
        }
        TokenTree::Group(group) => conditional_tokens(group.stream()),
        _ => false,
    })
}

/// Platform modules preserve a feature's message namespace. Crate-root
/// references use `app.*`; real feature modules append their Rust names.
fn runtime_api(parts: &[String]) -> bool {
    parts.len() == 1
        && matches!(
            parts[0].as_str(),
            "initialize" | "locales" | "catalogs" | "install" | "BUILD_ID" | "MESSAGE_COUNT"
        )
}

pub(crate) fn module_namespace(parent: &str, child: &str) -> String {
    if matches!(child, "client" | "server") {
        parent.to_owned()
    } else if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}.{child}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_runtime_api_and_aliases_are_not_message_references() {
        let result = extract(
            r#"
            use crate::t::{initialize as start, locales, BUILD_ID};
            fn boot() {
                start(locales());
                t::install(lang, bytes);
                let _ = (t::catalogs(), BUILD_ID, t::MESSAGE_COUNT);
                t::common::welcome(lang, "Amina");
            }
        "#,
            CatalogAudience::Browser,
        );
        assert!(result.extracted.diagnostics.is_empty());
        assert_eq!(result.extracted.references.len(), 1);
        assert_eq!(result.extracted.references[0].key, "common.welcome");
    }

    fn extract(source: &str, audience: CatalogAudience) -> RustExtraction {
        let target = if audience == CatalogAudience::Browser {
            "wasm32"
        } else {
            "x86_64"
        };
        let cfg = CfgSet::from_rustc(&format!("target_arch=\"{target}\"\n")).unwrap();
        extract_rust(
            source,
            &SourceContext {
                file: 7,
                module: String::new(),
                audience,
                offset: 0,
            },
            &cfg,
        )
    }

    #[test]
    fn follows_cfg_server_bodies_imports_and_inline_modules() {
        let source = r#"
            mod auth { mod server {
                #[server] fn login() { t::auth::invalid_credentials(locale); }
            }
            mod client {
                use crate::t::auth::{submit as label};
                fn render() { format!("{}", label(locale)); let _ = "t::auth::fake"; }
                #[cfg(not(target_arch="wasm32"))] fn host() { t::auth::host(locale); }
                fn more() {
                    #[cfg(not(target_arch="wasm32"))] { t::auth::hidden(locale); }
                }
            }}
        "#;
        let browser = extract(source, CatalogAudience::Browser);
        assert!(
            browser.extracted.diagnostics.is_empty(),
            "{:?}",
            browser.extracted.diagnostics
        );
        assert_eq!(
            browser
                .extracted
                .references
                .iter()
                .filter(|r| !matches!(r.kind, ReferenceKind::RustImport))
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["auth.submit"]
        );
        let host = extract(source, CatalogAudience::Host);
        assert_eq!(
            host.extracted
                .references
                .iter()
                .filter(|r| !matches!(r.kind, ReferenceKind::RustImport))
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            [
                "auth.invalid_credentials",
                "auth.submit",
                "auth.host",
                "auth.hidden"
            ]
        );
        assert!(host.extracted.references.iter().all(|r| r.module == "auth"));
    }

    #[test]
    fn templates_have_original_spans_and_disabled_components_are_ignored() {
        let source = r#"const EMOJI: &str="🎉";
            mod cart {
                #[component(template=poco! {<input :placeholder="$t.cart.search">})] struct Search;
                #[component] struct Cart;
                #[cfg(target_arch="other")] #[component(template="Missing.poco")] struct Hidden;
                #[path="alternate.rs"] mod details;
                mod nested { mod child; }
            }"#;
        let out = extract(source, CatalogAudience::Browser);
        assert!(
            out.extracted.diagnostics.is_empty(),
            "{:?}",
            out.extracted.diagnostics
        );
        assert_eq!(out.extracted.references[0].key, "cart.search");
        assert_eq!(
            out.extracted.references[0].span.start as usize,
            source.find("<input").unwrap()
        );
        assert_eq!(out.templates.len(), 1);
        assert_eq!(out.templates[0].path, "Cart.poco");
        assert_eq!(out.modules[0].path.as_deref(), Some("alternate.rs"));
        assert_eq!(out.modules[0].parents, ["cart"]);
        assert_eq!(out.modules[1].parents, ["cart", "nested"]);
        assert_eq!(out.modules[1].context.module, "cart.nested.child");
    }

    #[test]
    fn rust_errors_and_translation_globs_are_actionable() {
        for source in ["fn broken(", "use crate::t::auth::*; fn f() {}"] {
            assert!(
                !extract(source, CatalogAudience::Browser)
                    .extracted
                    .diagnostics
                    .is_empty()
            );
        }
    }

    #[test]
    fn conditional_statements_fields_methods_and_opaque_macros_do_not_leak() {
        let source = r#"
            mod auth {
                struct Options {
                    #[cfg(not(target_arch="wasm32"))]
                    #[prop(default=t::auth::secret(locale))] label: String,
                }
                impl Options {
                    #[cfg(not(target_arch="wasm32"))] fn secret() { t::auth::secret(locale); }
                    fn show() {
                        #[cfg(not(target_arch="wasm32"))] println!("{}",t::auth::secret(locale));
                    }
                }
            }
        "#;
        let out = extract(source, CatalogAudience::Browser);
        assert!(out.extracted.references.is_empty());
        assert!(out.extracted.diagnostics.is_empty());
        assert_eq!(
            extract(source, CatalogAudience::Host)
                .extracted
                .references
                .len(),
            3
        );
        let conditional = extract(
            r#"cfg_if! { if #[cfg(unix)] { fn f() {t::auth::secret(locale);} } }"#,
            CatalogAudience::Browser,
        );
        assert!(!conditional.extracted.diagnostics.is_empty());
    }

    #[test]
    fn quoted_inline_escapes_nested_macros_and_source_positions_match_expansion() {
        let source = r#"mod auth { fn f() { wrapper!(poco! {
            <p>"\u{7b}\u{7b} $t.auth.visible }}"</p>
            <p>"\\{{ $t.auth.escaped }}"</p>
            <p>"Étiquette 🎉"</p><input :title="$t.auth.title">
        }); } }"#;
        let out = extract(source, CatalogAudience::Browser);
        assert!(
            out.extracted.diagnostics.is_empty(),
            "{:?}",
            out.extracted.diagnostics
        );
        assert_eq!(
            out.extracted
                .references
                .iter()
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["auth.visible", "auth.title"]
        );
        assert_eq!(
            out.extracted.references[1].span.start as usize,
            source.find("<input").unwrap()
        );
        let unsupported = extract(
            r#"wrap! { #[component] struct Hidden; }"#,
            CatalogAudience::Browser,
        );
        assert!(!unsupported.extracted.diagnostics.is_empty());
    }

    #[test]
    fn raw_rust_identifiers_keep_the_catalog_key_spelling() {
        let out = extract(
            "mod r#type { fn label() {t::r#type::r#match(locale);} }",
            CatalogAudience::Host,
        );
        assert!(out.extracted.diagnostics.is_empty());
        assert_eq!(out.extracted.references[0].module, "type");
        assert_eq!(out.extracted.references[0].key, "type.match");
    }
}
