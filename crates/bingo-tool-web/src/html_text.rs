//! The words inside an HTML fragment. A search result's title and snippet
//! arrive as markup; what the model should read is the text.

/// A fragment as its words: tags dropped, entities resolved, whitespace
/// collapsed. Tags go first, so an escaped `&lt;b&gt;` survives as text.
pub(crate) fn plain(fragment: &str) -> String {
    let stripped = strip_tags(fragment);
    decode(&stripped)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Entities resolved, markup left alone — what a URL attribute needs.
pub(crate) fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let entity = &rest[start..];
        let (replacement, len) = read_entity(entity);
        out.push_str(&replacement);
        rest = &entity[len..];
    }
    out.push_str(rest);
    out
}

const NAMED: &[(&str, &str)] = &[
    ("&amp;", "&"),
    ("&quot;", "\""),
    ("&apos;", "'"),
    ("&lt;", "<"),
    ("&gt;", ">"),
    ("&nbsp;", " "),
];

/// What an entity stands for and how many bytes of it to consume. Text that
/// only looks like one consumes the ampersand alone, so nothing is lost.
fn read_entity(entity: &str) -> (String, usize) {
    if let Some((name, text)) = NAMED.iter().find(|(name, _)| entity.starts_with(name)) {
        return ((*text).to_string(), name.len());
    }
    numeric(entity).unwrap_or_else(|| ("&".to_string(), 1))
}

fn numeric(entity: &str) -> Option<(String, usize)> {
    let rest = entity.strip_prefix("&#")?;
    let end = rest.find(';')?;
    let digits = rest.get(..end)?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    Some((char::from_u32(code)?.to_string(), end + "&#;".len()))
}

fn strip_tags(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_entities_are_resolved() {
        assert_eq!(
            decode("a &amp; b &lt;c&gt; &quot;d&quot;"),
            "a & b <c> \"d\""
        );
        assert_eq!(decode("one&nbsp;two"), "one two");
    }

    #[test]
    fn numeric_entities_are_resolved_in_both_bases() {
        assert_eq!(decode("&#x27;quoted&#x27;"), "'quoted'");
        assert_eq!(decode("&#39;quoted&#39;"), "'quoted'");
        assert_eq!(decode("&#x2014;"), "—");
    }

    #[test]
    fn an_ampersand_that_is_not_an_entity_survives() {
        assert_eq!(decode("a & b"), "a & b");
        assert_eq!(decode("q=1&rut=2"), "q=1&rut=2");
        assert_eq!(decode("&#zz;"), "&#zz;");
    }

    #[test]
    fn tags_go_and_whitespace_collapses() {
        assert_eq!(plain("First <b>snippet</b>\n   text"), "First snippet text");
        assert_eq!(plain("&lt;script&gt; stays text"), "<script> stays text");
    }
}
