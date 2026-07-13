use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct Attrs {
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn main() {}
