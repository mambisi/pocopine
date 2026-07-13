use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct Attrs {
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, String>,
}

fn main() {}
