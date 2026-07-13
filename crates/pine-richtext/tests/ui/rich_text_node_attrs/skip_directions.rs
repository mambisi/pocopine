use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct SkipSerializingAttrs {
    #[serde(skip_serializing)]
    label: String,
}

#[derive(RichTextNodeAttrs)]
struct SkipDeserializingAttrs {
    #[serde(skip_deserializing)]
    label: String,
}

fn main() {}
