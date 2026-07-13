use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
struct SerializeAttrs {
    #[serde(serialize_with = "serialize_label")]
    label: String,
}

#[derive(RichTextNodeAttrs)]
struct DeserializeAttrs {
    #[serde(deserialize_with = "deserialize_label")]
    label: String,
}

#[derive(RichTextNodeAttrs)]
struct WithAttrs {
    #[serde(with = "label_serde")]
    label: String,
}

fn main() {}
