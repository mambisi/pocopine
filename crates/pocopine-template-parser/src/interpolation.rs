//! Shared compile-time template interpolation grammar (RFC-040).
//! Locale extraction and macro plans must agree on escapes and expression
//! boundaries. Static text retains its original UTF-8 characters.

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterpolationSegment {
    Static(String),
    Dynamic(String),
}

pub fn parse_interpolations(input: &str) -> Result<Vec<InterpolationSegment>, String> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut static_buf = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 2 < bytes.len() {
            let n1 = bytes[i + 1];
            let n2 = bytes[i + 2];
            if (n1 == b'{' && n2 == b'{') || (n1 == b'}' && n2 == b'}') {
                static_buf.push(n1 as char);
                static_buf.push(n2 as char);
                i += 3;
                continue;
            }
        }
        if b == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            static_buf.push('\\');
            i += 2;
            continue;
        }
        if b == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if !static_buf.is_empty() {
                out.push(InterpolationSegment::Static(std::mem::take(
                    &mut static_buf,
                )));
            }
            let start = i + 2;
            let mut j = start;
            let mut found = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b'}' && bytes[j + 1] == b'}' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                return Err("unclosed `{{` in text".into());
            }
            let src = std::str::from_utf8(&bytes[start..j])
                .map_err(|_| "non-UTF-8 text")?
                .trim()
                .to_string();
            if src.is_empty() {
                return Err("empty `{{}}` interpolation".into());
            }
            out.push(InterpolationSegment::Dynamic(src));
            i = j + 2;
            continue;
        }
        let character = input[i..].chars().next().expect("in-bounds UTF-8");
        static_buf.push(character);
        i += character.len_utf8();
    }
    if !static_buf.is_empty() {
        out.push(InterpolationSegment::Static(static_buf));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_escaped_delimiters_survive() {
        use InterpolationSegment::{Dynamic, Static};
        assert_eq!(
            parse_interpolations("Bonjour é🌲 {{ name }} — مرحبا").unwrap(),
            vec![
                Static("Bonjour é🌲 ".into()),
                Dynamic("name".into()),
                Static(" — مرحبا".into())
            ]
        );
        assert_eq!(
            parse_interpolations(r"\{{literal}} \\{{name}}").unwrap(),
            vec![Static("{{literal}} \\".into()), Dynamic("name".into())]
        );
        assert!(parse_interpolations("{{}}").is_err());
        assert!(parse_interpolations("{{missing").is_err());
    }
}
