//! A `#[component]` index for the language server: scans a crate's `.rs`
//! sources with `syn` and records each component's tag, field roles + handler
//! methods, and their source locations. The Rust counterpart of the extension's
//! TypeScript `ComponentRegistry` — used for symbol-aware `.poco` diagnostics,
//! completion, hover, and goto-definition.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use syn::{Fields, ImplItem, Item, Type, Visibility};

/// Zero-based source position (LSP coordinates).
#[derive(Clone, Copy, Debug, Default)]
pub struct Pos {
    pub line: u32,
    pub character: u32,
}

impl Pos {
    /// From a `proc_macro2` span start (1-based line, 0-based column).
    fn from_span(span: proc_macro2::Span) -> Self {
        let start = span.start();
        Pos {
            line: (start.line.saturating_sub(1)) as u32,
            character: start.column as u32,
        }
    }
}

/// `#[prop]` / `#[model]` field decorator role (RFC-006 / RFC-044), or plain
/// component state when neither is present.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldRole {
    Prop,
    Model,
    State,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub role: FieldRole,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub struct HandlerDecl {
    pub name: String,
    /// Handlers live in `impl` blocks that may be in a different file than the
    /// struct, so each carries its own file + position.
    pub file: PathBuf,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub struct ComponentDecl {
    /// PascalCase struct name.
    pub name: String,
    /// kebab-case tag derived from `name`.
    pub tag: String,
    /// File declaring the struct (fields live here too).
    pub file: PathBuf,
    /// Position of the struct name.
    pub pos: Pos,
    pub fields: Vec<FieldDecl>,
    pub handlers: Vec<HandlerDecl>,
}

impl ComponentDecl {
    pub fn field(&self, name: &str) -> Option<&FieldDecl> {
        self.fields.iter().find(|f| f.name == name)
    }
    pub fn handler(&self, name: &str) -> Option<&HandlerDecl> {
        self.handlers.iter().find(|h| h.name == name)
    }
    pub fn has_handler(&self, name: &str) -> bool {
        self.handlers.iter().any(|h| h.name == name)
    }
}

/// Workspace index keyed by both tag and struct name.
#[derive(Default, Debug)]
pub struct ComponentIndex {
    by_tag: HashMap<String, ComponentDecl>,
    by_name: HashMap<String, ComponentDecl>,
}

impl ComponentIndex {
    pub fn by_tag(&self, tag: &str) -> Option<&ComponentDecl> {
        self.by_tag.get(tag)
    }
    pub fn by_name(&self, name: &str) -> Option<&ComponentDecl> {
        self.by_name.get(name)
    }
    pub fn iter(&self) -> impl Iterator<Item = &ComponentDecl> {
        self.by_tag.values()
    }
}

/// Scan every `.rs` under `root` (skipping `target/` and hidden dirs) for
/// `#[component]` structs and their handler methods. Files that fail to read or
/// parse are skipped — a single broken file never sinks the whole index.
pub fn scan(root: &Path) -> ComponentIndex {
    let mut files = Vec::new();
    collect_rs(root, &mut files);

    let mut comps: HashMap<String, ComponentDecl> = HashMap::new();
    let mut handlers: HashMap<String, Vec<HandlerDecl>> = HashMap::new();

    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&src) else {
            continue;
        };
        for item in &file.items {
            match item {
                Item::Struct(s) if has_attr(&s.attrs, "component") => {
                    let name = s.ident.to_string();
                    comps.insert(
                        name.clone(),
                        ComponentDecl {
                            tag: to_kebab_case(&name),
                            name: name.clone(),
                            file: path.clone(),
                            pos: Pos::from_span(s.ident.span()),
                            fields: struct_fields(&s.fields),
                            handlers: Vec::new(),
                        },
                    );
                }
                Item::Impl(im) => {
                    if let Some(target) = impl_self_name(&im.self_ty) {
                        for impl_item in &im.items {
                            if let ImplItem::Fn(f) = impl_item {
                                if matches!(f.vis, Visibility::Public(_)) {
                                    handlers.entry(target.clone()).or_default().push(HandlerDecl {
                                        name: f.sig.ident.to_string(),
                                        file: path.clone(),
                                        pos: Pos::from_span(f.sig.ident.span()),
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut index = ComponentIndex::default();
    for (name, mut decl) in comps {
        if let Some(hs) = handlers.remove(&name) {
            decl.handlers = hs;
        }
        index.by_tag.insert(decl.tag.clone(), decl.clone());
        index.by_name.insert(name, decl);
    }
    index
}

fn struct_fields(fields: &Fields) -> Vec<FieldDecl> {
    let Fields::Named(named) = fields else {
        return Vec::new();
    };
    named
        .named
        .iter()
        .filter_map(|f| {
            let ident = f.ident.as_ref()?;
            Some(FieldDecl {
                name: ident.to_string(),
                role: field_role(&f.attrs),
                pos: Pos::from_span(ident.span()),
            })
        })
        .collect()
}

fn field_role(attrs: &[syn::Attribute]) -> FieldRole {
    // Last decorator wins, mirroring the TS registry.
    let mut role = FieldRole::State;
    for attr in attrs {
        if attr.path().is_ident("model") {
            role = FieldRole::Model;
        } else if attr.path().is_ident("prop") {
            role = FieldRole::Prop;
        }
    }
    role
}

fn has_attr(attrs: &[syn::Attribute], ident: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(ident))
}

/// The trailing ident of an `impl <Type>` self type (`impl Foo`, `impl x::Foo`).
fn impl_self_name(ty: &Type) -> Option<String> {
    if let Type::Path(tp) = ty {
        return tp.path.segments.last().map(|s| s.ident.to_string());
    }
    None
}

/// `PineDialogRoot` → `pine-dialog-root`. Matches the extension's `toKebabCase`.
pub fn to_kebab_case(pascal: &str) -> String {
    let mut out = String::with_capacity(pascal.len() + 4);
    let chars: Vec<char> = pascal.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            let after_lower_or_digit = prev
                .map(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                .unwrap_or(false);
            let acronym_boundary = prev.map(|p| p.is_ascii_uppercase()).unwrap_or(false)
                && next.map(|n| n.is_ascii_lowercase()).unwrap_or(false);
            if i > 0 && (after_lower_or_digit || acronym_boundary) {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
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
            if name == "target" || name.starts_with('.') {
                continue;
            }
            collect_rs(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_matches_extension() {
        assert_eq!(to_kebab_case("Counter"), "counter");
        assert_eq!(to_kebab_case("TodoItem"), "todo-item");
        assert_eq!(to_kebab_case("PineDialogRoot"), "pine-dialog-root");
        assert_eq!(to_kebab_case("HTMLBlock"), "html-block");
    }
}
