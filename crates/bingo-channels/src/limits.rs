//! What one platform will carry: how long a message may be, in whose units,
//! which markup it renders, and how many buttons fit under a question.
//!
//! "Length" is not one thing (ADR-0016 §1): Feishu counts a card's serialised
//! bytes, Telegram counts characters, Slack counts UTF-8 and Discord's client
//! counts UTF-16 units. A limit without its unit is a limit that is wrong on
//! three platforms out of four, so the unit travels with the number.

/// How a platform measures the length of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Chars,
    Utf8Bytes,
    Utf16Units,
}

impl Encoding {
    pub fn measure(self, text: &str) -> usize {
        match self {
            Encoding::Chars => text.chars().count(),
            Encoding::Utf8Bytes => text.len(),
            Encoding::Utf16Units => text.chars().map(char::len_utf16).sum(),
        }
    }
}

/// The markup a platform draws. `Plain` is not "markdown minus a renderer":
/// a fence drawn as literal backticks is worse than no fence at all, so the
/// dialect is applied to the text, never assumed away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Plain,
    Markdown,
}

impl Dialect {
    /// The text as this dialect wants it. `Markdown` passes through; `Plain`
    /// drops the fence lines, which are the only markup a chat renders as
    /// noise rather than as itself.
    pub fn render(self, text: &str) -> String {
        match self {
            Dialect::Markdown => text.to_string(),
            Dialect::Plain => text
                .lines()
                .filter(|line| !line.trim_start().starts_with("```"))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The longest one message may be, and the unit it is counted in.
    pub max_text: (usize, Encoding),
    pub dialect: Dialect,
    /// The most buttons one question may carry.
    pub max_actions: usize,
    /// The longest one button label may be, in characters.
    pub max_label: usize,
}

impl Limits {
    /// `text`, cut to fit, with an ellipsis where the cut was. A message that
    /// fits comes back untouched, so the common case allocates nothing new.
    pub fn clip<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        let (max, encoding) = self.max_text;
        if encoding.measure(text) <= max {
            return std::borrow::Cow::Borrowed(text);
        }
        let mark = '…';
        let room = max.saturating_sub(encoding.measure(mark.encode_utf8(&mut [0u8; 4])));
        let mut kept = String::new();
        for c in text.chars() {
            let mut buffer = [0u8; 4];
            let width = encoding.measure(c.encode_utf8(&mut buffer));
            if encoding.measure(&kept) + width > room {
                break;
            }
            kept.push(c);
        }
        kept.push(mark);
        std::borrow::Cow::Owned(kept)
    }

    /// A label a button will show: cut to `max_label` characters.
    pub fn label(&self, label: &str) -> String {
        if label.chars().count() <= self.max_label {
            return label.to_string();
        }
        let kept: String = label
            .chars()
            .take(self.max_label.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max: usize, encoding: Encoding) -> Limits {
        Limits {
            max_text: (max, encoding),
            dialect: Dialect::Markdown,
            max_actions: 4,
            max_label: 8,
        }
    }

    #[test]
    fn each_encoding_counts_the_same_string_differently() {
        let text = "a好𝄞";
        assert_eq!(Encoding::Chars.measure(text), 3);
        assert_eq!(Encoding::Utf8Bytes.measure(text), 1 + 3 + 4);
        assert_eq!(Encoding::Utf16Units.measure(text), 1 + 1 + 2);
    }

    #[test]
    fn a_message_that_fits_is_not_copied() {
        let limits = limits(10, Encoding::Chars);
        assert!(matches!(
            limits.clip("short"),
            std::borrow::Cow::Borrowed("short")
        ));
    }

    #[test]
    fn a_clip_never_exceeds_the_limit_in_its_own_unit() {
        for encoding in [Encoding::Chars, Encoding::Utf8Bytes, Encoding::Utf16Units] {
            let limits = limits(8, encoding);
            let clipped = limits.clip("好好好好好好好好好好");
            assert!(
                encoding.measure(&clipped) <= 8,
                "{encoding:?} let {clipped:?} through"
            );
            assert!(clipped.ends_with('…'));
        }
    }

    #[test]
    fn a_plain_dialect_drops_the_fences_and_keeps_the_code() {
        let text = "look:\n```rust\nfn main() {}\n```\ndone";
        assert_eq!(
            Dialect::Plain.render(text),
            "look:\nfn main() {}\ndone",
            "a fence a chat cannot draw is noise, not markup"
        );
        assert_eq!(Dialect::Markdown.render(text), text);
    }

    #[test]
    fn a_long_label_is_cut_with_an_ellipsis() {
        let limits = limits(100, Encoding::Chars);
        assert_eq!(limits.label("Allow"), "Allow");
        assert_eq!(limits.label("Allow for this session"), "Allow f…");
    }
}
