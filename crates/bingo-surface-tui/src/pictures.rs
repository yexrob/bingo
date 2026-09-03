//! The pictures a line carries, the way Claude Code and Codex spell them: a
//! paste puts `[image N]` in the line and the bytes are held here under `N`.
//!
//! The line is the record. What is sent is derived from the tokens still in
//! it at submit ([`Held::carried`]), so a token a person deletes takes its
//! picture with it and nothing has to remember that it did. `@shot.png` is
//! the other spelling — a word that names a file — and lives in
//! `complete::attachments`; this module is only the pasted kind.

use std::collections::BTreeMap;

use bingo_sdk::Image;

const OPEN: &str = "[image ";

/// `[image N]`, the words a pasted picture is in the line.
pub fn placeholder(n: u32) -> String {
    format!("{OPEN}{n}]")
}

/// The tokens in the line, in the order they appear; a `[image x]` that is
/// not a number is words.
pub fn tokens(line: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find(OPEN) {
        rest = &rest[at + OPEN.len()..];
        let Some(end) = rest.find(']') else { break };
        if let Ok(n) = rest[..end].parse::<u32>() {
            found.push(n);
        }
        rest = &rest[end + 1..];
    }
    found
}

/// The number the next paste takes: one past the highest in the line, so a
/// deleted token's number can come back but a live one is never reused.
pub fn next_token(line: &str) -> u32 {
    tokens(line).into_iter().max().map_or(1, |n| n + 1)
}

/// The pasted pictures behind the composer, by token.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Held {
    by_token: BTreeMap<u32, Image>,
}

impl Held {
    /// Keep `image` for the token a paste into `line` mints, and say which.
    pub fn hold(&mut self, line: &str, image: Image) -> u32 {
        let n = next_token(line);
        self.by_token.insert(n, image);
        n
    }

    /// What the line still carries, in token order; a token typed by hand
    /// with nothing held under it is words.
    pub fn carried(&self, line: &str) -> Vec<Image> {
        self.shown(line)
            .into_iter()
            .map(|(_, image)| image.clone())
            .collect()
    }

    /// The same, each under the token that names it: what the strip draws,
    /// which needs the token to know the picture by (M48 brick 3).
    pub fn shown(&self, line: &str) -> Vec<(u32, &Image)> {
        tokens(line)
            .into_iter()
            .filter_map(|n| Some((n, self.by_token.get(&n)?)))
            .collect()
    }

    /// The picture one token names, whatever the line says. What the send
    /// reads a drawn thumbnail back out of.
    pub fn under(&self, token: u32) -> Option<&Image> {
        self.by_token.get(&token)
    }

    pub fn clear(&mut self) {
        self.by_token.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(tag: &str) -> Image {
        Image::from_bytes("image/png", tag.as_bytes()).expect("a small picture")
    }

    #[test]
    fn tokens_are_read_in_order_and_words_are_not_tokens() {
        assert_eq!(tokens("see [image 2] and [image 1]"), vec![2, 1]);
        assert!(tokens("[image x] [image ] [image").is_empty());
        assert_eq!(tokens("[image 3"), Vec::<u32>::new());
    }

    #[test]
    fn the_next_token_is_one_past_the_highest() {
        assert_eq!(next_token(""), 1);
        assert_eq!(next_token("[image 1] then [image 3]"), 4);
    }

    #[test]
    fn a_paste_is_held_under_the_token_the_line_gets() {
        let mut held = Held::default();
        let n = held.hold("look", image("a"));
        assert_eq!(n, 1);
        let line = format!("look {}", placeholder(n));
        assert_eq!(held.carried(&line), vec![image("a")]);
    }

    #[test]
    fn a_deleted_token_drops_its_picture_and_order_follows_the_line() {
        let mut held = Held::default();
        held.hold("", image("a"));
        held.hold("[image 1]", image("b"));
        assert_eq!(
            held.carried("[image 2] [image 1]"),
            vec![image("b"), image("a")]
        );
        assert_eq!(held.carried("[image 2]"), vec![image("b")]);
        assert!(held.carried("nothing").is_empty());
    }

    #[test]
    fn a_token_typed_by_hand_is_words() {
        let held = Held::default();
        assert!(held.carried("[image 7]").is_empty());
        assert!(held.shown("[image 7]").is_empty());
    }

    /// The strip draws what the line carries and needs to know each one's
    /// token; the send reads one back out by that token alone.
    #[test]
    fn what_is_shown_carries_its_token_and_is_read_back_by_it() {
        let mut held = Held::default();
        held.hold("", image("a"));
        held.hold("[image 1]", image("b"));
        assert_eq!(
            held.shown("[image 2] and [image 1]"),
            vec![(2, &image("b")), (1, &image("a"))]
        );
        assert_eq!(held.under(1), Some(&image("a")));
        assert_eq!(held.under(9), None);
    }
}
