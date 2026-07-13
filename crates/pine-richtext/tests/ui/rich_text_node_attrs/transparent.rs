use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
#[serde(transparent)]
struct Attrs {
    label: String,
}

fn main() {}
