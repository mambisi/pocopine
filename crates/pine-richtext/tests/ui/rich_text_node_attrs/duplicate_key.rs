use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct Attrs {
    #[serde(rename = "label")]
    first: String,
    label: String,
}

fn main() {}
