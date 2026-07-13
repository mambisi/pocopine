//! Compile-time-proven component outlets for framework-owned child DOM.
//!
//! A component may give one native element in its template to an external
//! owner (an editor renderer is one example) by marking it with
//! `pp-owned-content`. The component macro strips that marker and implements
//! [`OwnedContentOutletComponent`] with an element-child path relative to the
//! rendered template root. Runtime consumers resolve that path directly; no
//! selector, id, class, or subtree scan is involved.

use std::fmt;

use web_sys::Element;

use crate::app::MountableComponent;

/// Macro-emitted proof that a component template has one stable owned-content
/// outlet.
///
/// The path counts only element children, matching the browser
/// `Element::children()` collection. An empty path names the rendered template
/// root itself. The path itself lives on
/// [`MountableComponent::OWNED_CONTENT_OUTLET_PATH`] so code with only a
/// mountable-component bound can inspect the optional metadata (notably to
/// reject outlets for atom renderers). This marker remains the compile-time
/// proof required by APIs that need an outlet.
#[doc(hidden)]
pub trait OwnedContentOutletComponent: MountableComponent {}

/// A failed compiled owned-content path resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedContentOutletError {
    /// A forged/manual marker implementation has no matching optional path
    /// metadata on its mountable-component contract.
    MissingMetadata { component: &'static str },
    /// The mount host does not contain a rendered template root.
    MissingTemplateRoot { component: &'static str },
    /// A compiled element-child index no longer exists in the mounted shell.
    MissingPathSegment {
        component: &'static str,
        path: &'static [u16],
        depth: usize,
        index: u16,
    },
    /// The compiled path resolved to a custom/framework element instead of a
    /// native HTML or SVG element.
    NonNativeTarget {
        component: &'static str,
        path: &'static [u16],
        tag: String,
    },
    /// The compiled path resolved outside the supported HTML/SVG namespaces.
    UnsupportedNamespace {
        component: &'static str,
        path: &'static [u16],
        namespace: Option<String>,
    },
}

impl fmt::Display for OwnedContentOutletError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMetadata { component } => write!(
                f,
                "pocopine: component `{component}` claims an owned-content outlet but has no compiled path metadata"
            ),
            Self::MissingTemplateRoot { component } => write!(
                f,
                "pocopine: mounted component `{component}` has no rendered template root"
            ),
            Self::MissingPathSegment {
                component,
                path,
                depth,
                index,
            } => write!(
                f,
                "pocopine: owned-content path {path:?} for component `{component}` failed at depth {depth} (missing element child {index})"
            ),
            Self::NonNativeTarget {
                component,
                path,
                tag,
            } => write!(
                f,
                "pocopine: owned-content path {path:?} for component `{component}` resolved to non-native `<{tag}>`"
            ),
            Self::UnsupportedNamespace {
                component,
                path,
                namespace,
            } => write!(
                f,
                "pocopine: owned-content path {path:?} for component `{component}` resolved in unsupported namespace {namespace:?}"
            ),
        }
    }
}

impl std::error::Error for OwnedContentOutletError {}

/// Resolve a component's compiled outlet from the element used as its mount
/// host.
///
/// The host may be the component's custom element or an arbitrary tooling
/// mount host accepted by [`crate::app::App::mount_subtree_with`]. Its first
/// element child is the rendered template root.
pub fn resolve_owned_content_outlet<C>(
    component_host: &Element,
) -> Result<Element, OwnedContentOutletError>
where
    C: OwnedContentOutletComponent,
{
    let root = component_host
        .children()
        .item(0)
        .ok_or(OwnedContentOutletError::MissingTemplateRoot { component: C::NAME })?;
    resolve_owned_content_outlet_from_root::<C>(&root)
}

/// Resolve a component's compiled outlet from its rendered template root.
///
/// This lower-level form is useful when the caller already retained the root
/// during mounting. Each hop uses the live `Element::children()` collection,
/// so text and comment nodes never affect the compiled indices.
pub fn resolve_owned_content_outlet_from_root<C>(
    component_root: &Element,
) -> Result<Element, OwnedContentOutletError>
where
    C: OwnedContentOutletComponent,
{
    let path = <C as MountableComponent>::OWNED_CONTENT_OUTLET_PATH
        .ok_or(OwnedContentOutletError::MissingMetadata { component: C::NAME })?;
    let mut current = component_root.clone();
    for (depth, &index) in path.iter().enumerate() {
        current = current.children().item(u32::from(index)).ok_or(
            OwnedContentOutletError::MissingPathSegment {
                component: C::NAME,
                path,
                depth,
                index,
            },
        )?;
    }

    let tag = current.local_name();
    if tag.contains('-') {
        return Err(OwnedContentOutletError::NonNativeTarget {
            component: C::NAME,
            path,
            tag,
        });
    }

    const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
    const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
    let namespace = current.namespace_uri();
    if !matches!(namespace.as_deref(), Some(HTML_NAMESPACE | SVG_NAMESPACE)) {
        return Err(OwnedContentOutletError::UnsupportedNamespace {
            component: C::NAME,
            path,
            namespace,
        });
    }

    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::OwnedContentOutletError;

    #[test]
    fn path_errors_name_the_component_and_exact_failed_hop() {
        let error = OwnedContentOutletError::MissingPathSegment {
            component: "task-item-view",
            path: &[1, 0],
            depth: 1,
            index: 0,
        };
        let message = error.to_string();
        assert!(message.contains("`task-item-view`"), "{message}");
        assert!(message.contains("[1, 0]"), "{message}");
        assert!(message.contains("depth 1"), "{message}");
        assert!(message.contains("element child 0"), "{message}");
    }
}
