//! The name an unnamed session earns from its first ask.
//!
//! A mint, not a rename: nothing here reads a session that already has a
//! title, so every explicit name — a seat's, a room's, one a client opened the
//! session with — outlives it. Both halves are pure, so what a name comes out
//! as is decided here and asserted here.

use bingo_sdk::{ContentPart, Item, ItemBody, Origin};

use super::commands;

/// How many characters of the ask a name keeps. Characters, not bytes: a cut
/// that is not at a boundary is not a cut.
const LIMIT: usize = 48;

/// What was said first in the session, if a person has said anything. Prose
/// only — an ask that carried nothing but an image names nothing.
pub fn first_ask(items: &[Item]) -> Option<&str> {
    items.iter().find_map(|item| match &item.body {
        ItemBody::User { parts, origin } if !from_a_command(origin) => parts.iter().find_map(prose),
        _ => None,
    })
}

/// Whether a user item is a command's own prompt rather than something asked.
/// `/guide` puts a whole skill body in the journal, and a session named after
/// that page is named after nothing anyone said (ADR-0008 §3).
fn from_a_command(origin: &Origin) -> bool {
    origin.surface == commands::SURFACE
}

fn prose(part: &ContentPart) -> Option<&str> {
    match part {
        ContentPart::Text { text } => Some(text.as_str()),
        _ => None,
    }
}

/// One ask as a name: its first line, its first sentence of that, and no more
/// of it than a row can carry.
pub fn mint(ask: &str) -> Option<String> {
    let sentence = sentence(ask.trim().lines().next()?.trim());
    match sentence.is_empty() {
        true => None,
        false => Some(shorten(sentence)),
    }
}

/// Up to the first full stop, so a name is the ask rather than the paragraph
/// behind it. A stop stops only where something follows it: `1.5` is a number
/// and `.rs` is a suffix.
fn sentence(line: &str) -> &str {
    let mut characters = line.char_indices().peekable();
    while let Some((at, character)) = characters.next() {
        let stops = match character {
            '。' | '！' | '？' => true,
            '.' | '!' | '?' => characters
                .peek()
                .is_none_or(|(_, next)| next.is_whitespace()),
            _ => false,
        };
        if stops {
            return line[..at].trim_end();
        }
    }
    line
}

fn shorten(sentence: &str) -> String {
    if sentence.chars().count() <= LIMIT {
        return sentence.to_string();
    }
    let head: String = sentence.chars().take(LIMIT).collect();
    format!("{}…", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{ItemId, ItemStatus, Origin};
    use jiff::Timestamp;

    fn item(body: ItemBody) -> Item {
        Item {
            id: ItemId::mint(),
            turn: None,
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::UNIX_EPOCH,
            completed_at: None,
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn asked(parts: Vec<ContentPart>) -> Item {
        item(ItemBody::User {
            parts,
            origin: Origin::surface("tui"),
        })
    }

    fn answered(text: &str) -> Item {
        item(ItemBody::Assistant { text: text.into() })
    }

    #[test]
    fn a_name_is_the_first_sentence_of_the_first_line() {
        assert_eq!(
            mint("Fix the parser. It crashes on unicode.\nAnd on tabs."),
            Some("Fix the parser".into())
        );
        assert_eq!(mint("  who am i?  "), Some("who am i".into()));
        assert_eq!(mint("no stop at all"), Some("no stop at all".into()));
    }

    #[test]
    fn a_stop_inside_a_word_is_not_a_stop() {
        assert_eq!(mint("bump it to 1.5"), Some("bump it to 1.5".into()));
        assert_eq!(mint("read main.rs"), Some("read main.rs".into()));
    }

    #[test]
    fn nothing_is_named_by_nothing() {
        assert_eq!(mint(""), None);
        assert_eq!(mint("   \n  "), None);
        assert_eq!(mint("."), None);
    }

    /// The cut is by character, so a name never ends in half a codepoint —
    /// which for a CJK first line is every cut there is.
    #[test]
    fn a_long_ask_is_cut_at_a_character_boundary() {
        let ask = "请帮我把这个解析器修好".repeat(8);
        let name = mint(&ask).expect("a name");
        assert_eq!(name.chars().count(), LIMIT + 1, "{name}");
        assert!(name.ends_with('…'));
        assert!(ask.starts_with(name.trim_end_matches('…')), "{name}");

        let ascii = "a".repeat(200);
        assert_eq!(mint(&ascii).expect("a name").chars().count(), LIMIT + 1);
    }

    #[test]
    fn a_cjk_sentence_ends_at_its_own_stop() {
        assert_eq!(
            mint("把解析器修好。再看看测试"),
            Some("把解析器修好".into())
        );
    }

    /// A skill's body is the command talking. It is a page long, it says
    /// nothing about why the session exists, and the ask after it does.
    #[test]
    fn a_commands_own_prompt_names_nothing() {
        let mut expansion = asked(vec![ContentPart::text(
            "/guide\n\nRead this before answering questions about bingo.",
        )]);
        if let ItemBody::User { origin, .. } = &mut expansion.body {
            origin.surface = "command".into();
        }
        assert_eq!(first_ask(&[expansion.clone()]), None);
        assert_eq!(
            first_ask(&[expansion, asked(vec![ContentPart::text("fix the parser")])]),
            Some("fix the parser")
        );
    }

    #[test]
    fn the_first_ask_is_the_first_prose_a_person_wrote() {
        let items = vec![
            answered("hello"),
            asked(vec![ContentPart::Image {
                media_type: "image/png".into(),
                data: "AA".into(),
            }]),
            asked(vec![ContentPart::text("fix the parser")]),
            asked(vec![ContentPart::text("and the lexer")]),
        ];
        assert_eq!(first_ask(&items), Some("fix the parser"));
        assert_eq!(first_ask(&[]), None);
        assert_eq!(first_ask(&[answered("hello")]), None);
    }
}
