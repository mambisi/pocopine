//! Default mark types: `link`, `em`, `strong`, `code`. Identical to
//! the marks pre-extension `schema_basic::schema()` declared.
//!
//! Phase 6 C4: this extension also contributes the markdown
//! emitters and parse rules for its marks via the pluggable
//! contract on [`RichTextExtension`]. Apps that drop
//! `CoreMarksExtension` (e.g. a comment runtime with custom
//! marks) won't get em/strong/link/code markdown handling
//! automatically — they have to register their own emitters /
//! rules. Apps using the default chain pick everything up for
//! free.

use std::sync::Arc;

use pulldown_cmark::{Event as MdEvent, LinkType, Tag as MdTag};
use serde_json::{Value, json};

use crate::extension::RichTextExtension;
use crate::markdown::{
    MarkEmitter, MarkRender, MarkdownParseRule, ParseMapping, ParseMatch, TagKind,
};
use crate::model::{Attrs, MarkSpec};

pub struct CoreMarksExtension;

impl RichTextExtension for CoreMarksExtension {
    fn name(&self) -> &str {
        "core_marks"
    }

    fn marks(&self) -> Vec<MarkSpec> {
        vec![
            MarkSpec::new("link")
                .required_attr("href")
                .attr("title", Value::Null)
                .inclusive(false),
            MarkSpec::new("em"),
            MarkSpec::new("strong"),
            MarkSpec::new("code").excludes("_"),
        ]
    }

    fn markdown_mark_emitters(&self) -> Vec<(String, MarkEmitter)> {
        vec![
            (
                "em".into(),
                Arc::new(|_mark| MarkRender::Wrap(MdTag::Emphasis)),
            ),
            (
                "strong".into(),
                Arc::new(|_mark| MarkRender::Wrap(MdTag::Strong)),
            ),
            (
                "link".into(),
                Arc::new(|mark| {
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
                    MarkRender::Wrap(MdTag::Link {
                        link_type: LinkType::Inline,
                        dest_url: href.into(),
                        title: title.into(),
                        id: "".into(),
                    })
                }),
            ),
            (
                "code".into(),
                Arc::new(|_mark| {
                    MarkRender::Atomic(Arc::new(|text: &str| {
                        MdEvent::Code(text.to_string().into())
                    }))
                }),
            ),
        ]
    }

    fn markdown_parse_rules(&self) -> Vec<MarkdownParseRule> {
        vec![
            MarkdownParseRule {
                matches: ParseMatch::Tag(TagKind::Emphasis),
                maps_to: ParseMapping::Mark {
                    mark_type: "em".into(),
                    get_attrs: None,
                },
            },
            MarkdownParseRule {
                matches: ParseMatch::Tag(TagKind::Strong),
                maps_to: ParseMapping::Mark {
                    mark_type: "strong".into(),
                    get_attrs: None,
                },
            },
            MarkdownParseRule {
                matches: ParseMatch::Tag(TagKind::Link),
                maps_to: ParseMapping::Mark {
                    mark_type: "link".into(),
                    get_attrs: Some(Arc::new(|event| {
                        let mut attrs = Attrs::new();
                        if let MdEvent::Start(MdTag::Link {
                            dest_url, title, ..
                        }) = event
                        {
                            attrs.insert("href".into(), json!(dest_url.to_string()));
                            if !title.is_empty() {
                                attrs.insert("title".into(), json!(title.to_string()));
                            }
                        }
                        attrs
                    })),
                },
            },
        ]
    }
}
