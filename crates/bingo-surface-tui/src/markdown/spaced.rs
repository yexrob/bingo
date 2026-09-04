//! A picture whose path has a space in it. CommonMark ends a destination at
//! the first space, so `![a shot](my shots/a.png)` is not a picture to the
//! parser but seven words of text — and a model writes paths the way a shell
//! shows them, spaces and all. The destination is put in the angle brackets
//! CommonMark wants before the parser sees it; a document that has no such
//! picture is returned untouched.

use std::borrow::Cow;

/// The text with every bare, spaced picture destination bracketed.
pub fn mended(text: &str) -> Cow<'_, str> {
    if !text.contains("![") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut fenced = false;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if is_fence(line) {
            fenced = !fenced;
        }
        match fenced {
            true => out.push_str(line),
            false => out.push_str(&mended_line(line)),
        }
    }
    Cow::Owned(out)
}

/// A line that opens or closes a fenced block: nothing in one is a picture.
fn is_fence(line: &str) -> bool {
    let line = line.trim_start_matches(' ');
    line.starts_with("```") || line.starts_with("~~~")
}

/// One line, its pictures mended. A code span's contents are left alone.
fn mended_line(line: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut rest = line;
    let mut in_code = false;
    while let Some(at) = rest.find(['`', '!']) {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        if let Some(after) = from.strip_prefix('`') {
            in_code = !in_code;
            out.push('`');
            rest = after;
            continue;
        }
        match (!in_code).then(|| picture(from)).flatten() {
            Some((mended, after)) => {
                out.push_str(&mended);
                rest = after;
            }
            None => {
                out.push('!');
                rest = &from[1..];
            }
        }
    }
    if out.is_empty() {
        return Cow::Borrowed(line);
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The picture at the head of `from`, with its destination bracketed, and
/// what follows it — when there is one there and it needs mending.
fn picture(from: &str) -> Option<(String, &str)> {
    let alt_end = from.strip_prefix("![")?.find("](")? + 2;
    let dest_start = alt_end + 2;
    let dest_len = from[dest_start..].find(')')?;
    let dest = &from[dest_start..dest_start + dest_len];
    let (path, title) = split_title(dest);
    if path.starts_with('<') || !path.contains(' ') {
        return None;
    }
    let mended = format!("{}<{path}>{title})", &from[..dest_start]);
    Some((mended, &from[dest_start + dest_len + 1..]))
}

/// A destination's path and the quoted title after it, if it carries one:
/// `a b.png "the title"` is a spaced path and a title, not a longer path.
fn split_title(dest: &str) -> (&str, &str) {
    let dest = dest.trim_end();
    for quote in ["\"", "'"] {
        if dest.ends_with(quote)
            && let Some(at) = dest[..dest.len() - 1].rfind(&format!(" {quote}"))
        {
            return (&dest[..at], &dest[at..]);
        }
    }
    (dest, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spaced_path_is_bracketed() {
        assert_eq!(
            mended("see ![a shot](my shots/a b.png) here"),
            "see ![a shot](<my shots/a b.png>) here"
        );
    }

    #[test]
    fn an_unspaced_or_bracketed_path_is_left_alone() {
        for text in [
            "![a](a.png)",
            "![a](<my shots/a.png>)",
            "![a](https://x.dev/a.png)",
            "no picture here",
        ] {
            assert!(matches!(mended(text), Cow::Borrowed(_)) || mended(text) == text);
        }
    }

    #[test]
    fn a_title_stays_outside_the_brackets() {
        assert_eq!(
            mended("![a](my shots/a.png \"the title\")"),
            "![a](<my shots/a.png> \"the title\")"
        );
    }

    #[test]
    fn code_is_not_mended() {
        let fenced = "```\n![a](my shots/a.png)\n```";
        assert_eq!(mended(fenced), fenced);
        let span = "`![a](my shots/a.png)`";
        assert_eq!(mended(span), span);
    }

    #[test]
    fn two_pictures_on_one_line_are_both_mended() {
        assert_eq!(
            mended("![a](x y.png) and ![b](p q.png)"),
            "![a](<x y.png>) and ![b](<p q.png>)"
        );
    }

    #[test]
    fn a_bang_that_opens_no_picture_is_kept() {
        let text = "wow! ![a](b c.png) ![no close";
        assert_eq!(mended(text), "wow! ![a](<b c.png>) ![no close");
    }
}
