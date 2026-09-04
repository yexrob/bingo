//! The pictures a line carries, the way Claude Code and Codex spell them: a
//! paste puts `[image N]` in the line and the bytes are held here under `N`.
//!
//! The line is the record. What is sent is derived from the tokens still in
//! it at submit ([`Held::carried`]), so a token a person deletes takes its
//! picture with it and nothing has to remember that it did. `@shot.png` is
//! the other spelling — a word that names a file — and lives in
//! `complete::attachments`; this module is only the pasted kind.

use std::collections::BTreeMap;
use std::ops::Range;

use bingo_sdk::Image;

const OPEN: &str = "[image ";

/// `[image N]`, the words a pasted picture is in the line.
pub fn placeholder(n: u32) -> String {
    format!("{OPEN}{n}]")
}

/// The tokens in the line, in the order they appear; a `[image x]` that is
/// not a number is words.
pub fn tokens(line: &str) -> Vec<u32> {
    spans(line).into_iter().map(|(_, n)| n).collect()
}

/// Where each token sits in the line, as byte ranges, with its number.
fn spans(line: &str) -> Vec<(Range<usize>, u32)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = line[from..].find(OPEN) {
        let start = from + at;
        let digits = start + OPEN.len();
        let Some(close) = line[digits..].find(']') else {
            break;
        };
        let end = digits + close + 1;
        if let Ok(n) = line[digits..digits + close].parse::<u32>() {
            found.push((start..end, n));
        }
        from = end;
    }
    found
}

/// The start of the token whose last byte is at `at`, when the caret stands
/// right after one. A token is one thing in the line: a backspace or a step
/// left at its end takes all of it, never its closing bracket alone.
pub fn ending_at(line: &str, at: usize) -> Option<usize> {
    spans(line)
        .into_iter()
        .find(|(span, _)| span.end == at)
        .map(|(span, _)| span.start)
}

/// The end of the token whose first byte is at `at`: what a delete or a step
/// right from just before one covers.
pub fn starting_at(line: &str, at: usize) -> Option<usize> {
    spans(line)
        .into_iter()
        .find(|(span, _)| span.start == at)
        .map(|(span, _)| span.end)
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

    /// A line taken back out of the queue: the pictures it went out with go
    /// back under the tokens it still names, in the order they were sent
    /// ([`Held::carried`] is what put them in that order). A picture an
    /// `@word` named is not held here and needs no place — the word is still
    /// in the line, and it reads again when the line goes (M68).
    pub fn restore(&mut self, line: &str, images: Vec<Image>) {
        self.by_token = tokens(line).into_iter().zip(images).collect();
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

    /// A token's edges are known from the line, so an editor can treat it as
    /// one thing; a caret anywhere else — inside it, beside words — sees none.
    #[test]
    fn a_tokens_edges_are_found_from_either_end() {
        let line = "see [image 12] now";
        assert_eq!(ending_at(line, 14), Some(4));
        assert_eq!(starting_at(line, 4), Some(14));
        assert_eq!(ending_at(line, 13), None, "inside is not an edge");
        assert_eq!(starting_at(line, 5), None);
        assert_eq!(ending_at(line, line.len()), None, "words are words");
        assert_eq!(ending_at("[image x]", 9), None, "and so is a non-token");
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

    /// A line withdrawn from the queue comes back whole: the tokens are still
    /// in the words, and the pictures go back under them in the order they
    /// were sent, so sending it again sends exactly what was queued (M68).
    /// A picture an `@word` named rides past the tokens and needs no place.
    #[test]
    fn a_withdrawn_line_gets_its_pictures_back_under_the_tokens_it_names() {
        let mut held = Held::default();
        held.hold("", image("a"));
        held.hold("[image 1]", image("b"));
        let line = "look [image 2] and [image 1] and @shot.png";
        let sent = held.carried(line);
        assert_eq!(sent, vec![image("b"), image("a")]);

        let mut back = Held::default();
        back.restore(line, [sent, vec![image("mentioned")]].concat());
        assert_eq!(back.carried(line), vec![image("b"), image("a")]);
        assert_eq!(back.under(1), Some(&image("a")));
        assert_eq!(back.under(3), None, "and nothing is held for the word");
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
