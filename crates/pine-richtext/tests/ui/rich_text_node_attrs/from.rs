use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
#[serde(from = "LegacyAttrs")]
struct Attrs {
    label: String,
}

struct LegacyAttrs;

fn main() {}
