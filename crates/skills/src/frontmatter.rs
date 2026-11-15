//! A deliberately small frontmatter reader.
//!
//! Skill and command files start with a `---` block of `key: value` lines. That
//! is the only part of YAML this project needs, so there is no YAML dependency:
//! nested maps, anchors, multi-line scalars and typed values are not supported.
//! Anything unrecognised is kept as a plain string and simply never read.

use std::collections::BTreeMap;

/// Split a document into its frontmatter map and the remaining body.
///
/// A file with no frontmatter, or with an unterminated one, yields an empty map
/// and the original text as body — a broken header should never hide the prose.
pub fn split(text: &str) -> (BTreeMap<String, String>, String) {
    let mut map = BTreeMap::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return (map, String::new());
    };
    if first.trim() != "---" {
        return (map, text.to_string());
    }

    let mut body_start = None;
    for (offset, line) in lines.by_ref().enumerate() {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            body_start = Some(offset);
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }
            map.insert(key, unquote(value.trim()));
        }
    }

    let Some(closing) = body_start else {
        return (BTreeMap::new(), text.to_string());
    };

    // `closing` is the index of the terminator within the iterator that had
    // already consumed the opening line, so skip that many lines again.
    let body = text
        .lines()
        .skip(closing + 2)
        .collect::<Vec<_>>()
        .join("\n");
    (map, body)
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    // Strip a trailing ` # comment` outside of quotes.
    match value.split_once(" #") {
        Some((head, _)) => head.trim_end().to_string(),
        None => value.to_string(),
    }
}

/// `allowed-tools: Read, Grep` and `allowed-tools: [Read, Grep]` both parse.
pub fn split_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .flat_map(|part| part.split_whitespace())
        .map(|part| part.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_skill_header() {
        let text = "---\nname: pdf-tools\ndescription: Fill in PDF forms\n---\n# Body\nsteps";
        let (meta, body) = split(text);
        assert_eq!(meta.get("name").unwrap(), "pdf-tools");
        assert_eq!(meta.get("description").unwrap(), "Fill in PDF forms");
        assert_eq!(body.trim(), "# Body\nsteps");
    }

    #[test]
    fn quoted_values_and_comments() {
        let text = "---\nname: \"a: b\"\ndescription: hi # trailing\n---\nx";
        let (meta, _) = split(text);
        assert_eq!(meta.get("name").unwrap(), "a: b");
        assert_eq!(meta.get("description").unwrap(), "hi");
    }

    #[test]
    fn missing_frontmatter_keeps_the_whole_body() {
        let text = "# Just markdown\nno header here";
        let (meta, body) = split(text);
        assert!(meta.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn unterminated_frontmatter_is_not_swallowed() {
        let text = "---\nname: broken\nstill going";
        let (meta, body) = split(text);
        assert!(meta.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn list_values_parse_in_both_styles() {
        assert_eq!(split_list("Read, Grep"), vec!["Read", "Grep"]);
        assert_eq!(split_list("[Read, Grep]"), vec!["Read", "Grep"]);
        assert!(split_list("").is_empty());
    }
}
