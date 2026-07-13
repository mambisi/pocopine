//! Closed structural DOM fallback for semantic tag atoms.
//!
//! The typed Pocopine component remains the interactive browser view when the
//! `view` feature is installed. This native spec is the model-only renderer and
//! the visible fallback if that component cannot mount. It deliberately has no
//! remove button or other ephemeral UI state.

use pine_richtext::render::{DomAttrBinding, DomOutputSpec, NodeDomSpec};

use super::{TagKind, TagNode};

pub(super) const TAG_CLASS: &str = "pine-richtext-tag";
pub(super) const TAG_NATIVE_CLASS: &str = "pine-richtext-tag--native";
pub(super) const TAG_PREFIX_CLASS: &str = "pine-richtext-tag__prefix";
pub(super) const TAG_LABEL_CLASS: &str = "pine-richtext-tag__label";
pub(super) const TAG_MARKER_ATTR: &str = "data-pine-tag";

pub(super) fn dom_specs() -> Vec<NodeDomSpec> {
    vec![NodeDomSpec::nested::<TagNode>(
        DomOutputSpec::element("span")
            .class(format!("{TAG_CLASS} {TAG_NATIVE_CLASS}"))
            .attr(TAG_MARKER_ATTR, "true")
            .attr("contenteditable", "false")
            .attr("role", "group")
            .attr("aria-roledescription", "tag")
            .bind_attr(DomAttrBinding::string("data-tag-id", "id"))
            .bind_attr(DomAttrBinding::string("aria-label", "label"))
            .bind_attr(DomAttrBinding::token(
                "data-kind",
                "kind",
                [
                    TagKind::Neutral.as_str(),
                    TagKind::Info.as_str(),
                    TagKind::Success.as_str(),
                    TagKind::Warning.as_str(),
                    TagKind::Danger.as_str(),
                ],
            ))
            .child(
                DomOutputSpec::element("span")
                    .class(TAG_PREFIX_CLASS)
                    .attr("aria-hidden", "true")
                    .text("#"),
            )
            .child(
                DomOutputSpec::element("span")
                    .class(TAG_LABEL_CLASS)
                    .attr("aria-hidden", "true")
                    .bind_text("label"),
            ),
    )]
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use super::*;

    #[test]
    fn fallback_is_a_typed_inline_span() {
        let views = dom_specs();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].node_type(), "tag");
        assert_eq!(views[0].root_tag(), "span");
        assert_eq!(views[0].semantic_type_id(), TypeId::of::<TagNode>());
    }
}
