//! Builder for [`super::EditorRuntime`].
//!
//! Mirrors the schema-fold logic in `schema_basic::schema()` and the
//! base-extension table population in
//! `extension::registry::install_base_extensions`, but writes into a
//! per-runtime bundle instead of process-global `OnceLock`s. The user-
//! name-wins overlay semantics carry over: a custom extension whose
//! `name()` matches a default-set extension *replaces* the default in
//! fold position, preserving node-insertion rank for content-match
//! resolution.

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use crate::extension::{KeyBindings, NamedCommand, RichTextExtension};
use crate::extensions::default_extensions;
use crate::inputrules::{InputRule, input_rules as input_rules_plugin};
use crate::markdown::{
    MarkEmitter as MarkdownMarkEmitter, MarkdownParseRule, NodeEmitter as MarkdownNodeEmitter,
    ParseMatch as MarkdownParseMatch,
};
use crate::model::Schema;
use crate::serialization::{MarkdownPolicy, NodeSerializationSpec};
use crate::state::Plugin;
use crate::typed_nodes::TypedNodeSpec;
#[cfg(feature = "view")]
use crate::view::typed_node_views::{
    NodeViewHost as TypedNodeViewHost, NodeViewKind as TypedNodeViewKind,
    NodeViewSpec as TypedNodeViewSpec, RichTextViewExtension, TypedNodeViewRegistry,
};

use super::EditorRuntime;

type ExtChain = Vec<Arc<dyn RichTextExtension>>;

#[cfg(feature = "view")]
struct ViewContribution {
    extension_index: usize,
    extension: String,
    specs: Vec<TypedNodeViewSpec>,
}

/// Failure while folding an extension chain into an [`EditorRuntime`].
///
/// Runtime construction used to panic (or silently let a later node-view
/// binding replace an earlier one). External node views are loaded from
/// independently-authored crates, so those failure modes are not actionable
/// enough: callers need the runtime, extension, semantic node, and offending
/// component/tag before an editor is mounted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeBuildError {
    /// The composed node/mark schema is invalid.
    InvalidSchema {
        runtime: Option<String>,
        error: String,
    },
    /// A typed semantic descriptor's Rust name and `NodeSpec` disagree.
    TypedNodeNameMismatch {
        runtime: Option<String>,
        extension: String,
        semantic_rust_type: String,
        typed_name: String,
        spec_name: String,
    },
    /// Typed semantic nodes start at wire version 1.
    InvalidTypedNodeVersion {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        version: u32,
    },
    /// The closed serde key set and model attribute schema differ.
    TypedNodeAttrKeysMismatch {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        typed_keys: Vec<String>,
        spec_keys: Vec<String>,
    },
    /// `$pine_` is reserved for framework wire metadata.
    ReservedTypedNodeAttr {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        key: String,
    },
    /// A typed node's migration chain is incomplete or non-adjacent.
    InvalidTypedNodeMigration {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        expected_from: u32,
        found: Option<(u32, u32)>,
    },
    /// Two effective contributions use the same persisted semantic name.
    DuplicateSemanticNode {
        runtime: Option<String>,
        node_type: String,
        first_extension: String,
        second_extension: String,
    },
    /// A native DOM view references no typed semantic descriptor.
    UnknownDomViewNode {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        semantic_rust_type: String,
    },
    /// A DOM view used a different Rust marker sharing the same wire name.
    DomViewTypeMismatch {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        schema_semantic_rust_type: String,
        view_semantic_rust_type: String,
    },
    /// Two effective extensions claim one semantic native DOM view.
    DuplicateDomView {
        runtime: Option<String>,
        node_type: String,
        first_extension: String,
        second_extension: String,
    },
    /// A declarative DOM output violates the safe structural contract.
    InvalidDomView {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        error: String,
    },
    /// A serialization policy references no typed semantic descriptor.
    UnknownNodeSerialization {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        semantic_rust_type: String,
    },
    /// A policy used a different Rust marker sharing the same wire name.
    NodeSerializationTypeMismatch {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        schema_semantic_rust_type: String,
        policy_semantic_rust_type: String,
    },
    /// Two effective extensions claim one exact typed-node output contract.
    DuplicateNodeSerialization {
        runtime: Option<String>,
        node_type: String,
        first_extension: String,
        second_extension: String,
    },
    /// A typed node omitted one or more required output lanes.
    IncompleteNodeSerialization {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        missing: Vec<String>,
    },
    /// A typed node has no output contract at all.
    MissingNodeSerialization {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        semantic_rust_type: String,
    },
    /// A closed HTML/text policy violates its typed semantic contract.
    InvalidNodeSerialization {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        error: String,
    },
    /// `MarkdownPolicy::Supported` requires an actual semantic emitter.
    MissingTypedMarkdownEmitter {
        runtime: Option<String>,
        extension: String,
        node_type: String,
    },
    /// A typed component view references a semantic marker absent from this
    /// runtime.
    #[cfg(feature = "view")]
    UnknownTypedNodeView {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        semantic_rust_type: String,
    },
    /// A view used a different Rust marker that merely shares the same wire
    /// name.
    #[cfg(feature = "view")]
    TypedNodeViewTypeMismatch {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        schema_semantic_rust_type: String,
        view_semantic_rust_type: String,
    },
    /// Two view extensions claim the same semantic node.
    #[cfg(feature = "view")]
    DuplicateTypedNodeView {
        runtime: Option<String>,
        node_type: String,
        first_extension: String,
        second_extension: String,
    },
    /// Atom/editable component ownership disagrees with the semantic schema.
    #[cfg(feature = "view")]
    TypedNodeViewKindMismatch {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        kind: TypedNodeViewKind,
        atom: bool,
        content: String,
    },
    /// The component's compiled content outlet disagrees with atom/editable
    /// ownership.
    #[cfg(feature = "view")]
    TypedNodeViewOwnershipMismatch {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        component: String,
        kind: TypedNodeViewKind,
        outlet_path: Option<Vec<u16>>,
    },
    /// A component requested the semantic native host without contributing a
    /// native DOM spec whose root can supply it.
    #[cfg(feature = "view")]
    TypedNodeViewNativeHostMissing {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        component: String,
    },
    /// The component could not be registered through its typed mount ABI.
    #[cfg(feature = "view")]
    TypedNodeViewRegistration {
        runtime: Option<String>,
        extension: String,
        node_type: String,
        component: String,
        error: String,
    },
}

impl fmt::Display for RuntimeBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSchema {
                runtime: name,
                error,
            } => write!(
                f,
                "pine-richtext: runtime `{}` has an invalid composed schema: {error}",
                runtime_label(name)
            ),
            Self::TypedNodeNameMismatch {
                runtime: name,
                extension,
                semantic_rust_type,
                typed_name,
                spec_name,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{semantic_rust_type}` declares NAME `{typed_name}` but its NodeSpec is `{spec_name}`",
                runtime_label(name)
            ),
            Self::InvalidTypedNodeVersion {
                runtime: name,
                extension,
                node_type,
                version,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` has invalid version {version}; typed nodes start at version 1",
                runtime_label(name)
            ),
            Self::TypedNodeAttrKeysMismatch {
                runtime: name,
                extension,
                node_type,
                typed_keys,
                spec_keys,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` has serde keys {typed_keys:?}, but its NodeSpec declares {spec_keys:?}",
                runtime_label(name)
            ),
            Self::ReservedTypedNodeAttr {
                runtime: name,
                extension,
                node_type,
                key,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` uses reserved attr `{key}`; `$pine_` keys are framework metadata",
                runtime_label(name)
            ),
            Self::InvalidTypedNodeMigration {
                runtime: name,
                extension,
                node_type,
                expected_from,
                found,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` requires migration {expected_from}->{}, found {found:?}",
                runtime_label(name),
                expected_from + 1
            ),
            Self::DuplicateSemanticNode {
                runtime: name,
                node_type,
                first_extension,
                second_extension,
            } => write!(
                f,
                "pine-richtext: runtime `{}` defines semantic node `{node_type}` more than once (extensions `{first_extension}` and `{second_extension}`)",
                runtime_label(name)
            ),
            Self::UnknownDomViewNode {
                runtime: name,
                extension,
                node_type,
                semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` registers native DOM view `{semantic_rust_type}` for unknown typed node `{node_type}`",
                runtime_label(name)
            ),
            Self::DomViewTypeMismatch {
                runtime: name,
                extension,
                node_type,
                schema_semantic_rust_type,
                view_semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` native DOM view for `{node_type}` uses Rust marker `{view_semantic_rust_type}`, but schema registered `{schema_semantic_rust_type}`",
                runtime_label(name)
            ),
            Self::DuplicateDomView {
                runtime: name,
                node_type,
                first_extension,
                second_extension,
            } => write!(
                f,
                "pine-richtext: runtime `{}` has duplicate native DOM views for `{node_type}` (extensions `{first_extension}` and `{second_extension}`)",
                runtime_label(name)
            ),
            Self::InvalidDomView {
                runtime: name,
                extension,
                node_type,
                error,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` has invalid native DOM view for `{node_type}`: {error}",
                runtime_label(name)
            ),
            Self::UnknownNodeSerialization {
                runtime: name,
                extension,
                node_type,
                semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` registers serialization policy `{semantic_rust_type}` for unknown typed node `{node_type}`",
                runtime_label(name)
            ),
            Self::NodeSerializationTypeMismatch {
                runtime: name,
                extension,
                node_type,
                schema_semantic_rust_type,
                policy_semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` serialization policy for `{node_type}` uses Rust marker `{policy_semantic_rust_type}`, but schema registered `{schema_semantic_rust_type}`",
                runtime_label(name)
            ),
            Self::DuplicateNodeSerialization {
                runtime: name,
                node_type,
                first_extension,
                second_extension,
            } => write!(
                f,
                "pine-richtext: runtime `{}` has duplicate serialization policies for `{node_type}` (extensions `{first_extension}` and `{second_extension}`)",
                runtime_label(name)
            ),
            Self::IncompleteNodeSerialization {
                runtime: name,
                extension,
                node_type,
                missing,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` has incomplete serialization policy; missing {missing:?}",
                runtime_label(name)
            ),
            Self::MissingNodeSerialization {
                runtime: name,
                extension,
                node_type,
                semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` (`{semantic_rust_type}`) has no explicit serialization policy",
                runtime_label(name)
            ),
            Self::InvalidNodeSerialization {
                runtime: name,
                extension,
                node_type,
                error,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` typed node `{node_type}` has invalid serialization policy: {error}",
                runtime_label(name)
            ),
            Self::MissingTypedMarkdownEmitter {
                runtime: name,
                extension,
                node_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` declares Markdown support for typed node `{node_type}` but contributes no node emitter",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::UnknownTypedNodeView {
                runtime: name,
                extension,
                node_type,
                semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` registers component view `{semantic_rust_type}` for unknown typed semantic node `{node_type}`",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::TypedNodeViewTypeMismatch {
                runtime: name,
                extension,
                node_type,
                schema_semantic_rust_type,
                view_semantic_rust_type,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` component view for `{node_type}` uses Rust marker `{view_semantic_rust_type}`, but the schema registered `{schema_semantic_rust_type}`",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::DuplicateTypedNodeView {
                runtime: name,
                node_type,
                first_extension,
                second_extension,
            } => write!(
                f,
                "pine-richtext: runtime `{}` has duplicate typed component views for `{node_type}` (extensions `{first_extension}` and `{second_extension}`)",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::TypedNodeViewKindMismatch {
                runtime: name,
                extension,
                node_type,
                kind,
                atom,
                content,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` registers {kind:?} component ownership for `{node_type}`, but the semantic schema has atom={atom} and content `{content}`",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::TypedNodeViewOwnershipMismatch {
                runtime: name,
                extension,
                node_type,
                component,
                kind,
                outlet_path,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` registers {kind:?} component `{component}` for `{node_type}` with owned-content path {outlet_path:?}",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::TypedNodeViewNativeHostMissing {
                runtime: name,
                extension,
                node_type,
                component,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` component `{component}` requests the native host for `{node_type}`, but no typed native DOM view is registered",
                runtime_label(name)
            ),
            #[cfg(feature = "view")]
            Self::TypedNodeViewRegistration {
                runtime: name,
                extension,
                node_type,
                component,
                error,
            } => write!(
                f,
                "pine-richtext: runtime `{}` extension `{extension}` could not register component `{component}` for typed node `{node_type}`: {error}",
                runtime_label(name)
            ),
        }
    }
}

impl std::error::Error for RuntimeBuildError {}

fn runtime_label(name: &Option<String>) -> &str {
    name.as_deref().unwrap_or("<default>")
}

/// Fluent builder for [`EditorRuntime`]. Created via
/// [`EditorRuntime::builder`] or [`RuntimeBuilder::new`].
///
/// ```ignore
/// let comment = RuntimeBuilder::new()
///     .name("comment")
///     .without_defaults()
///     .with(CoreNodesExtension)
///     .with(CoreInlineExtension)
///     .with(CoreMarksExtension)
///     .build();
/// ```
pub struct RuntimeBuilder {
    name: Option<String>,
    include_defaults: bool,
    extensions: Vec<Arc<dyn RichTextExtension>>,
    #[cfg(feature = "view")]
    view_contributions: Vec<ViewContribution>,
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            name: None,
            include_defaults: true,
            extensions: Vec::new(),
            #[cfg(feature = "view")]
            view_contributions: Vec::new(),
        }
    }

    /// Attach a diagnostic label. Surfaces in [`EditorRuntime::name`]
    /// and in `tracing::warn!` collision messages.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add an extension. Extensions added later in fold order shadow
    /// earlier ones only if names collide; otherwise both contribute.
    pub fn with<E: RichTextExtension>(mut self, ext: E) -> Self {
        self.extensions
            .push(Arc::new(ext) as Arc<dyn RichTextExtension>);
        self
    }

    /// Add both the target-independent model contract and its typed browser
    /// views from one extension value.
    #[cfg(feature = "view")]
    pub fn with_view<E>(mut self, ext: E) -> Self
    where
        E: RichTextExtension + RichTextViewExtension,
    {
        let extension_index = self.extensions.len();
        let extension = ext.name().to_string();
        let specs = ext.typed_node_views();
        self.extensions
            .push(Arc::new(ext) as Arc<dyn RichTextExtension>);
        self.view_contributions.push(ViewContribution {
            extension_index,
            extension,
            specs,
        });
        self
    }

    /// Add a pre-boxed extension. Useful when a trait object comes
    /// from a generic context that already produced `Box<dyn ...>`.
    pub fn with_boxed(mut self, ext: Box<dyn RichTextExtension>) -> Self {
        self.extensions.push(Arc::from(ext));
        self
    }

    /// Add a pre-Arc'd extension. Used by `runtime::default()` to
    /// bridge user extensions registered via the legacy
    /// `extension::registry::register` path (which stores
    /// `Arc<dyn RichTextExtension>` already) without losing the
    /// shared reference or re-boxing.
    pub fn with_arc(mut self, ext: Arc<dyn RichTextExtension>) -> Self {
        self.extensions.push(ext);
        self
    }

    /// Opt out of the kitchen-sink default extension chain. Use this
    /// for editors that should NOT have headings/lists/task-items —
    /// e.g. a comment box with only `paragraph` + basic marks.
    pub fn without_defaults(mut self) -> Self {
        self.include_defaults = false;
        self
    }

    /// Test-only: build the runtime with an explicit base extension
    /// list instead of `default_extensions()`. Lets tests pin
    /// base-vs-user precedence contracts (e.g. "user rules fire
    /// before base rules in the merged input-rules list") by
    /// injecting a known stub into the base chain.
    #[cfg(test)]
    pub(crate) fn build_with_explicit_base_for_tests(
        self,
        explicit_base: Vec<Arc<dyn RichTextExtension>>,
    ) -> Arc<EditorRuntime> {
        self.build_inner(Some(explicit_base))
            .expect("explicit test runtime is valid")
    }

    /// Fold the extension chain into an immutable [`EditorRuntime`].
    /// Wraps in `Arc` so multiple mounts can share one fold.
    pub fn build(self) -> Arc<EditorRuntime> {
        self.try_build()
            .expect("pine-richtext runtime construction failed")
    }

    /// Fallibly fold the extension chain into an immutable runtime.
    ///
    /// Prefer this for plugin/external-block runtimes: unlike [`Self::build`],
    /// it reports schema and node-view registration problems before any editor
    /// DOM is mounted.
    pub fn try_build(self) -> Result<Arc<EditorRuntime>, RuntimeBuildError> {
        self.build_inner(None)
    }

    /// Internal fold. Production path calls `build_inner(None)` to
    /// use `default_extensions()` as the base chain; tests can pass
    /// an explicit override.
    fn build_inner(
        self,
        explicit_base: Option<Vec<Arc<dyn RichTextExtension>>>,
    ) -> Result<Arc<EditorRuntime>, RuntimeBuildError> {
        let RuntimeBuilder {
            name: runtime_name,
            include_defaults,
            extensions: raw_user,
            #[cfg(feature = "view")]
            view_contributions,
        } = self;

        // Step 1: dedup the user list first-wins by name, matching
        // `extension::registry::register`'s semantics. A repeated
        // `.with(MyExt).with(MyExt)` keeps the first and warns about
        // the second, instead of folding both into the schema and
        // crashing `Schema::builder().finish()` on duplicate node-types.
        let mut seen_user_names: HashSet<String> = HashSet::new();
        let mut kept_user_indices = HashSet::new();
        let user: ExtChain = raw_user
            .into_iter()
            .enumerate()
            .filter_map(|(index, arc)| {
                if !seen_user_names.insert(arc.name().to_string()) {
                    tracing::warn!(
                        target: "pocopine.log",
                        "pine-richtext: runtime builder dropping duplicate extension `{}` (first-wins)",
                        arc.name()
                    );
                    return None;
                }
                kept_user_indices.insert(index);
                Some(arc)
            })
            .collect();

        // Step 2: resolve the effective schema-fold chain with user-name-wins
        // overlay. For each default-set extension, swap in any user
        // extension that shares its name(); then append remaining user
        // extensions in registration order. This preserves node-insertion
        // rank for content-match resolution (the same way
        // `schema_basic::schema()` does today).
        let (base_arcs, effective): (ExtChain, ExtChain) = if include_defaults
            || explicit_base.is_some()
        {
            let base: ExtChain = explicit_base.unwrap_or_else(|| {
                default_extensions()
                    .into_iter()
                    .map(|boxed| -> Arc<dyn RichTextExtension> { Arc::from(boxed) })
                    .collect()
            });
            let base_names: HashSet<String> = base.iter().map(|e| e.name().to_string()).collect();

            let mut effective: ExtChain = Vec::with_capacity(base.len() + user.len());
            for base_ext in &base {
                let replacement = user.iter().find(|u| u.name() == base_ext.name());
                effective.push(replacement.cloned().unwrap_or_else(|| base_ext.clone()));
            }
            for user_ext in &user {
                if !base_names.contains(user_ext.name()) {
                    effective.push(user_ext.clone());
                }
            }
            (base, effective)
        } else {
            (Vec::new(), user.clone())
        };

        // Step 3: fold node + mark specs into a schema using the overlay
        // order. Typed semantic nodes travel through the same model schema,
        // but retain their Rust TypeId/version/decoder in a parallel runtime
        // registry after their closed-map contract is validated.
        let mut typed_nodes: HashMap<String, TypedNodeSpec> = HashMap::new();
        let mut typed_node_owners: HashMap<TypeId, String> = HashMap::new();
        let mut semantic_node_owners: HashMap<String, String> = HashMap::new();
        let schema = {
            let mut builder = Schema::builder();
            for ext in &effective {
                for spec in ext.nodes() {
                    if let Some(first_extension) =
                        semantic_node_owners.insert(spec.name().to_string(), ext.name().to_string())
                    {
                        return Err(RuntimeBuildError::DuplicateSemanticNode {
                            runtime: runtime_name.clone(),
                            node_type: spec.name().to_string(),
                            first_extension,
                            second_extension: ext.name().to_string(),
                        });
                    }
                    builder = builder.node(spec);
                }
                for typed in ext.typed_nodes() {
                    validate_typed_node(&runtime_name, ext.name(), &typed)?;
                    let node_type = typed.name().to_string();
                    if let Some(first_extension) =
                        semantic_node_owners.insert(node_type.clone(), ext.name().to_string())
                    {
                        return Err(RuntimeBuildError::DuplicateSemanticNode {
                            runtime: runtime_name.clone(),
                            node_type,
                            first_extension,
                            second_extension: ext.name().to_string(),
                        });
                    }
                    builder = builder.typed_node(typed.clone());
                    typed_node_owners.insert(typed.semantic_type_id(), ext.name().to_string());
                    typed_nodes.insert(typed.name().to_string(), typed);
                }
                for spec in ext.marks() {
                    builder = builder.mark(spec);
                }
            }
            builder
                .finish()
                .map_err(|error| RuntimeBuildError::InvalidSchema {
                    runtime: runtime_name.clone(),
                    error: error.to_string(),
                })?
        };

        // Fold one complete serialization contract per exact semantic
        // `TypeId`. Nothing is inferred from the legacy Markdown/native-DOM
        // contribution tables: typed nodes must opt every format in or out.
        let mut node_serialization: HashMap<TypeId, NodeSerializationSpec> = HashMap::new();
        let mut serialization_owners: HashMap<TypeId, String> = HashMap::new();
        for ext in &effective {
            for policy in ext.node_serialization() {
                let node_type = policy.node_type().to_string();
                let Some(typed) = typed_nodes.get(&node_type) else {
                    return Err(RuntimeBuildError::UnknownNodeSerialization {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type,
                        semantic_rust_type: policy.semantic_rust_type().to_string(),
                    });
                };
                if typed.semantic_type_id() != policy.semantic_type_id() {
                    return Err(RuntimeBuildError::NodeSerializationTypeMismatch {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type,
                        schema_semantic_rust_type: typed.semantic_rust_type().to_string(),
                        policy_semantic_rust_type: policy.semantic_rust_type().to_string(),
                    });
                }
                let missing = policy.missing_policies();
                if !missing.is_empty() {
                    return Err(RuntimeBuildError::IncompleteNodeSerialization {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type,
                        missing: missing.into_iter().map(str::to_string).collect(),
                    });
                }
                policy
                    .validate(typed.attr_keys(), typed.spec().is_atom())
                    .map_err(|error| RuntimeBuildError::InvalidNodeSerialization {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type: node_type.clone(),
                        error: error.to_string(),
                    })?;
                if let Some(first_extension) =
                    serialization_owners.insert(policy.semantic_type_id(), ext.name().to_string())
                {
                    return Err(RuntimeBuildError::DuplicateNodeSerialization {
                        runtime: runtime_name.clone(),
                        node_type,
                        first_extension,
                        second_extension: ext.name().to_string(),
                    });
                }
                node_serialization.insert(policy.semantic_type_id(), policy);
            }
        }
        for typed in typed_nodes.values() {
            if !node_serialization.contains_key(&typed.semantic_type_id()) {
                return Err(RuntimeBuildError::MissingNodeSerialization {
                    runtime: runtime_name.clone(),
                    extension: typed_node_owners
                        .get(&typed.semantic_type_id())
                        .cloned()
                        .unwrap_or_else(|| "<unknown>".to_string()),
                    node_type: typed.name().to_string(),
                    semantic_rust_type: typed.semantic_rust_type().to_string(),
                });
            }
        }

        // Fold deterministic native/fallback DOM views. These are paired by
        // exact semantic TypeId and validated before rendering can begin.
        let mut dom_views = HashMap::new();
        let mut dom_view_owners: HashMap<String, String> = HashMap::new();
        for ext in &effective {
            for view in ext.dom_views() {
                let node_type = view.node_type().to_string();
                let Some(typed) = typed_nodes.get(&node_type) else {
                    return Err(RuntimeBuildError::UnknownDomViewNode {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type,
                        semantic_rust_type: view.semantic_rust_type().to_string(),
                    });
                };
                if typed.semantic_type_id() != view.semantic_type_id() {
                    return Err(RuntimeBuildError::DomViewTypeMismatch {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type,
                        schema_semantic_rust_type: typed.semantic_rust_type().to_string(),
                        view_semantic_rust_type: view.semantic_rust_type().to_string(),
                    });
                }
                view.validate(typed.attr_keys(), typed.spec().is_atom())
                    .map_err(|error| RuntimeBuildError::InvalidDomView {
                        runtime: runtime_name.clone(),
                        extension: ext.name().to_string(),
                        node_type: node_type.clone(),
                        error: error.to_string(),
                    })?;
                if let Some(first_extension) =
                    dom_view_owners.insert(node_type.clone(), ext.name().to_string())
                {
                    return Err(RuntimeBuildError::DuplicateDomView {
                        runtime: runtime_name.clone(),
                        node_type,
                        first_extension,
                        second_extension: ext.name().to_string(),
                    });
                }
                dom_views.insert(node_type, view);
            }
        }

        // Step 4: fold runtime tables. Each table has its own precedence
        // rule, mirroring the legacy `extension::registry::*` contracts:
        //
        //   * commands / key bindings — **user first, then base**, first-
        //     wins (user can override base by reusing a command key).
        //     Matches `named_command` and `merged_keymap_factories`.
        //   * plugins — **base first, then user**, first-wins on
        //     `plugin.key()`. Built-in plugins (e.g. `history`) protect
        //     their state field from accidental user shadowing.
        //     Matches `merged_plugins`'s documented contract.
        //   * list-item types — union (`HashSet`).
        let mut commands: HashMap<String, NamedCommand> = HashMap::new();
        let mut key_bindings: KeyBindings = Vec::new();
        let mut list_item_types: HashSet<String> = HashSet::new();
        let mut input_rules: Vec<InputRule> = Vec::new();
        let mut markdown_node_emitters: HashMap<String, MarkdownNodeEmitter> = HashMap::new();
        let mut markdown_mark_emitters: HashMap<String, MarkdownMarkEmitter> = HashMap::new();
        let mut markdown_parse_rules: HashMap<MarkdownParseMatch, Arc<MarkdownParseRule>> =
            HashMap::new();

        // Iteration order: user extensions FIRST, then base
        // extensions whose name isn't shadowed by a user extension.
        // First-wins for commands / keymaps / list-item-types means
        // user contributions override built-in ones. Input rules
        // are first-MATCH-wins (a typing-time regex against the
        // cursor-adjacent text), so this same ordering means
        // user-contributed rules take precedence over built-in
        // rules when their patterns overlap. Documented contract:
        // `.with(MyExt)` registers rules ahead of any base rules
        // the runtime ships, AND ahead of any non-shadowing user
        // extensions added later (registration-order wins among
        // user extensions).
        for ext in user.iter().chain(
            base_arcs
                .iter()
                .filter(|b| !user.iter().any(|u| u.name() == b.name())),
        ) {
            for (key, factory) in ext.commands() {
                commands.entry(key).or_insert(factory);
            }
            for binding in ext.key_bindings() {
                key_bindings.push(binding);
            }
            for &name in ext.list_item_types() {
                list_item_types.insert(name.to_string());
            }
            for rule in ext.input_rules() {
                input_rules.push(rule);
            }
            // Markdown emitters: same user-first / first-wins
            // semantics as commands. A user extension's emitter
            // for `task_item` shadows the default
            // TaskListExtension's entry for the same node type
            // because user extensions are iterated first.
            for (type_name, emitter) in ext.markdown_node_emitters() {
                markdown_node_emitters.entry(type_name).or_insert(emitter);
            }
            for (type_name, emitter) in ext.markdown_mark_emitters() {
                markdown_mark_emitters.entry(type_name).or_insert(emitter);
            }
            for rule in ext.markdown_parse_rules() {
                markdown_parse_rules
                    .entry(rule.matches)
                    .or_insert_with(|| Arc::new(rule));
            }
        }

        for policy in node_serialization.values() {
            if policy.markdown_policy() == Some(MarkdownPolicy::Supported)
                && !markdown_node_emitters.contains_key(policy.node_type())
            {
                return Err(RuntimeBuildError::MissingTypedMarkdownEmitter {
                    runtime: runtime_name.clone(),
                    extension: serialization_owners
                        .get(&policy.semantic_type_id())
                        .cloned()
                        .unwrap_or_else(|| "<unknown>".to_string()),
                    node_type: policy.node_type().to_string(),
                });
            }
        }

        #[cfg(feature = "view")]
        let mut node_view_owners: HashMap<String, String> = HashMap::new();

        // Typed component views prove the exact semantic marker, ownership
        // kind, and component mount ABI before an editor host exists.
        #[cfg(feature = "view")]
        let mut typed_node_views = TypedNodeViewRegistry::default();
        #[cfg(feature = "view")]
        for contribution in view_contributions
            .into_iter()
            .filter(|contribution| kept_user_indices.contains(&contribution.extension_index))
        {
            for spec in contribution.specs {
                let node_type = spec.node_type().to_string();
                let Some(typed) = typed_nodes.get(&node_type) else {
                    return Err(RuntimeBuildError::UnknownTypedNodeView {
                        runtime: runtime_name.clone(),
                        extension: contribution.extension.clone(),
                        node_type,
                        semantic_rust_type: spec.semantic_rust_type().to_string(),
                    });
                };
                if typed.semantic_type_id() != spec.semantic_type_id() {
                    return Err(RuntimeBuildError::TypedNodeViewTypeMismatch {
                        runtime: runtime_name.clone(),
                        extension: contribution.extension.clone(),
                        node_type,
                        schema_semantic_rust_type: typed.semantic_rust_type().to_string(),
                        view_semantic_rust_type: spec.semantic_rust_type().to_string(),
                    });
                }
                let atom = typed.spec().is_atom();
                let content = typed.spec().content_expression();
                let kind_matches = match spec.kind() {
                    TypedNodeViewKind::Atom => atom,
                    TypedNodeViewKind::Editable => !atom && !content.trim().is_empty(),
                };
                if !kind_matches {
                    return Err(RuntimeBuildError::TypedNodeViewKindMismatch {
                        runtime: runtime_name.clone(),
                        extension: contribution.extension.clone(),
                        node_type,
                        kind: spec.kind(),
                        atom,
                        content: content.to_string(),
                    });
                }
                let outlet_matches = match spec.kind() {
                    TypedNodeViewKind::Atom => spec.owned_content_path().is_none(),
                    TypedNodeViewKind::Editable => spec.owned_content_path().is_some(),
                };
                if !outlet_matches {
                    return Err(RuntimeBuildError::TypedNodeViewOwnershipMismatch {
                        runtime: runtime_name.clone(),
                        extension: contribution.extension.clone(),
                        node_type,
                        component: spec.component_name().to_string(),
                        kind: spec.kind(),
                        outlet_path: spec.owned_content_path().map(<[u16]>::to_vec),
                    });
                }
                if spec.host() == TypedNodeViewHost::Native && !dom_views.contains_key(&node_type) {
                    return Err(RuntimeBuildError::TypedNodeViewNativeHostMissing {
                        runtime: runtime_name.clone(),
                        extension: contribution.extension.clone(),
                        node_type,
                        component: spec.component_name().to_string(),
                    });
                }
                if let Some(first_extension) =
                    node_view_owners.insert(node_type.clone(), contribution.extension.clone())
                {
                    return Err(RuntimeBuildError::DuplicateTypedNodeView {
                        runtime: runtime_name.clone(),
                        node_type,
                        first_extension,
                        second_extension: contribution.extension.clone(),
                    });
                }
                spec.register_component().map_err(|error| {
                    RuntimeBuildError::TypedNodeViewRegistration {
                        runtime: runtime_name.clone(),
                        extension: contribution.extension.clone(),
                        node_type: node_type.clone(),
                        component: spec.component_name().to_string(),
                        error: error.to_string(),
                    }
                })?;
                typed_node_views.insert(spec);
            }
        }

        // Plugins: base first, then user. Same dedupe pattern as
        // `extension::registry::merged_plugins`. The input-rules
        // plugin is always included up-front so the rule-fire meta
        // path + `undo_input_rule` work for every runtime,
        // regardless of which extensions are registered. Its key
        // (`pine_richtext_input_rules`) is reserved.
        let mut plugins: Vec<Plugin> = vec![input_rules_plugin()];
        let mut seen_plugin_keys: HashSet<String> =
            plugins.iter().map(|p| p.key().to_string()).collect();
        let plugin_order = base_arcs
            .iter()
            .filter(|b| !user.iter().any(|u| u.name() == b.name()))
            .chain(user.iter());
        for ext in plugin_order {
            for plugin in ext.plugins() {
                if !seen_plugin_keys.insert(plugin.key().to_string()) {
                    tracing::warn!(
                        target: "pocopine.log",
                        "pine-richtext: plugin key `{}` already registered, keeping earlier binding (extension `{}` lost the race)",
                        plugin.key(),
                        ext.name()
                    );
                    continue;
                }
                plugins.push(plugin);
            }
        }

        let wire_descriptor = schema.wire_descriptor();
        let wire_fingerprint = pocopine_crypto::sha256_hex(
            &serde_json::to_vec(&wire_descriptor)
                .expect("wire-schema descriptor contains only serializable values"),
        );

        Ok(Arc::new(EditorRuntime {
            name: runtime_name,
            schema,
            extensions: effective,
            commands,
            key_bindings,
            plugins,
            list_item_types,
            input_rules,
            markdown_node_emitters,
            markdown_mark_emitters,
            markdown_parse_rules,
            typed_nodes,
            node_serialization,
            dom_views,
            wire_descriptor,
            wire_fingerprint,
            #[cfg(feature = "view")]
            typed_node_views,
        }))
    }
}

fn validate_typed_node(
    runtime: &Option<String>,
    extension: &str,
    typed: &TypedNodeSpec,
) -> Result<(), RuntimeBuildError> {
    if typed.name() != typed.spec().name() {
        return Err(RuntimeBuildError::TypedNodeNameMismatch {
            runtime: runtime.clone(),
            extension: extension.to_string(),
            semantic_rust_type: typed.semantic_rust_type().to_string(),
            typed_name: typed.name().to_string(),
            spec_name: typed.spec().name().to_string(),
        });
    }
    if typed.version() == 0 {
        return Err(RuntimeBuildError::InvalidTypedNodeVersion {
            runtime: runtime.clone(),
            extension: extension.to_string(),
            node_type: typed.name().to_string(),
            version: typed.version(),
        });
    }

    let mut typed_keys = typed
        .attr_keys()
        .iter()
        .map(|key| (*key).to_string())
        .collect::<Vec<_>>();
    typed_keys.sort();
    let spec_keys = typed.spec().attrs().keys().cloned().collect::<Vec<_>>();
    if typed_keys != spec_keys {
        return Err(RuntimeBuildError::TypedNodeAttrKeysMismatch {
            runtime: runtime.clone(),
            extension: extension.to_string(),
            node_type: typed.name().to_string(),
            typed_keys,
            spec_keys,
        });
    }
    if let Some(key) = typed
        .attr_keys()
        .iter()
        .find(|key| key.starts_with("$pine_"))
    {
        return Err(RuntimeBuildError::ReservedTypedNodeAttr {
            runtime: runtime.clone(),
            extension: extension.to_string(),
            node_type: typed.name().to_string(),
            key: (*key).to_string(),
        });
    }

    let migrations = typed.migrations();
    for expected_from in 1..typed.version() {
        let found = migrations
            .get((expected_from - 1) as usize)
            .map(|migration| (migration.from, migration.to));
        if found != Some((expected_from, expected_from + 1)) {
            return Err(RuntimeBuildError::InvalidTypedNodeMigration {
                runtime: runtime.clone(),
                extension: extension.to_string(),
                node_type: typed.name().to_string(),
                expected_from,
                found,
            });
        }
    }
    if migrations.len() != typed.version().saturating_sub(1) as usize {
        let expected_from = typed.version();
        let found = migrations
            .get(expected_from.saturating_sub(1) as usize)
            .map(|migration| (migration.from, migration.to));
        return Err(RuntimeBuildError::InvalidTypedNodeMigration {
            runtime: runtime.clone(),
            extension: extension.to_string(),
            node_type: typed.name().to_string(),
            expected_from,
            found,
        });
    }

    Ok(())
}
