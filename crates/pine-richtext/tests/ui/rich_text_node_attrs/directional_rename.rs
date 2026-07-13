use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
struct Attrs {
    display_name: String,
}

fn main() {}
