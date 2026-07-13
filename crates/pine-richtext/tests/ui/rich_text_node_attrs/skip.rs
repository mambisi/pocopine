use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct Attrs {
    #[serde(skip)]
    transient: String,
}

fn main() {}
