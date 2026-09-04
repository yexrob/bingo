//! The thumbnails of the pictures the draft is carrying, standing on the
//! input box's top border in rows of their own (outside the box since 2026-09-04,
//! at the user's word: the box keeps its height whatever is pasted).
//!
//! `[image 2]` stays in the words — it is the record, and the anchor in the
//! sentence, so deleting it takes its picture with it (M45). The strip is
//! that record made visible: the same pictures, in the same order, small
//! enough to glance at. It is derived from the line and from nothing else, so
//! there is nothing to keep in step.
//!
//! The rows themselves are [`crate::graphics::band`]'s, which is also what a
//! person's own `>` block wears once the line is sent (M62): the strip is the
//! band of what the draft carries, and this module is the reading of the line
//! that says which pictures those are.
//!
//! Only a terminal that draws pictures gets one. Everywhere else the box is
//! exactly what it was: the tokens in the line already say what is attached.

use bingo_sdk::Image;

use crate::graphics::picture::Source;
use crate::graphics::{Band, Decoded, Graphics, band};
use crate::pictures::Held;

/// The strip for a line, `width` columns wide.
pub fn rows(held: &Held, line: &str, graphics: Graphics, decoded: &Decoded, width: u16) -> Band {
    let Graphics::Kitty { cell, .. } = graphics else {
        return Band::default();
    };
    let carried: Vec<(Source, &Image)> = held
        .shown(line)
        .into_iter()
        .map(|(token, image)| (Source::Draft { token }, image))
        .collect();
    band::of(&carried, cell, decoded, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics;
    use crate::graphics::band::{ROWS, SHOWN};
    use bingo_pictures::testing::png;

    /// The pictures of a line, held under the tokens it names.
    fn holding(pictures: &[(u32, u32)]) -> (Held, String) {
        let mut held = Held::default();
        let mut line = String::new();
        for (width, height) in pictures {
            let token = held.hold(&line, png(*width, *height));
            line.push_str(&crate::pictures::placeholder(token));
        }
        (held, line)
    }

    fn strip(pictures: &[(u32, u32)], width: u16) -> Band {
        let (held, line) = holding(pictures);
        rows(
            &held,
            &line,
            graphics::drawing(),
            &Decoded::default(),
            width,
        )
    }

    /// What the line carries is what the strip draws, at the band's own size.
    #[test]
    fn a_carried_picture_is_a_thumbnail_of_the_band() {
        let strip = strip(&[(100, 200)], 60);
        assert_eq!(strip.height(), ROWS);
        assert_eq!(strip.pictures.len(), 1);
        assert_eq!((strip.pictures[0].cols, strip.pictures[0].rows), (3, 3));
        assert!(matches!(
            strip.pictures[0].source,
            Source::Draft { token: 1 }
        ));
    }

    /// Four are shown and the rest are counted, which is the band's rule read
    /// through the line.
    #[test]
    fn four_are_shown_and_the_rest_are_counted() {
        let strip = strip(&[(100, 100); 6], 60);
        assert_eq!(strip.pictures.len(), SHOWN);
        let last = &strip.lines[usize::from(ROWS) - 1];
        assert_eq!(
            last.spans.last().map(|span| span.content.as_ref()),
            Some("+2")
        );
    }

    /// The token order is the line's, not the paste order: a picture moved in
    /// the sentence moves in the strip.
    #[test]
    fn the_strip_is_in_the_lines_own_order() {
        let mut held = Held::default();
        held.hold("", png(100, 100));
        held.hold("[image 1]", png(200, 200));
        let drawn = rows(
            &held,
            "[image 2] then [image 1]",
            graphics::drawing(),
            &Decoded::default(),
            60,
        );
        let tokens: Vec<&Source> = drawn.pictures.iter().map(|one| &one.source).collect();
        assert_eq!(
            tokens,
            vec![&Source::Draft { token: 2 }, &Source::Draft { token: 1 }]
        );
    }

    /// A line that names nothing held has no strip, and neither has one on a
    /// terminal that draws no pictures.
    #[test]
    fn a_line_with_no_picture_and_a_terminal_with_none_have_no_strip() {
        assert_eq!(strip(&[], 60), Band::default());
        assert_eq!(strip(&[], 60).height(), 0);
        let (held, line) = holding(&[(100, 100)]);
        let off = rows(&held, &line, Graphics::Off, &Decoded::default(), 60);
        assert_eq!(off, Band::default());
        let typed = rows(
            &Held::default(),
            "[image 1]",
            graphics::drawing(),
            &Decoded::default(),
            60,
        );
        assert_eq!(typed, Band::default(), "a token typed by hand is words");
    }

    /// A narrow box shows fewer of them and still shows one: whether there is
    /// a band is a question about the line, so the rows the frame made room
    /// for are the rows that get drawn.
    #[test]
    fn a_narrow_box_cuts_thumbnails_and_never_the_band() {
        for (width, shown) in [(60u16, 4usize), (30, 2), (13, 1), (4, 1), (0, 1)] {
            let strip = strip(&[(100, 100); 4], width);
            assert_eq!(strip.pictures.len(), shown, "{width} columns");
            assert_eq!(strip.height(), ROWS, "{width} columns");
        }
    }
}
