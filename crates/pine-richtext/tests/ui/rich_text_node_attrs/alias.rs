use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct Attrs {
    #[serde(alias = "old-label")]
    label: String,
}

fn main() {}
