//! Default mark types: `link`, `em`, `strong`, `code`. Identical to
//! the marks pre-extension `schema_basic::schema()` declared.

use serde_json::Value;

use crate::extension::RichTextExtension;
use crate::model::MarkSpec;

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
}
