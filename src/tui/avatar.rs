//! Sender avatars: eight bundled portraits placed as kitty images.
//!
//! An avatar is not a new rendering mechanism — it is the D42 image path used at
//! chip size. The portrait is transmitted once per id and the cells it occupies are
//! ordinary styled text (the placeholder character, its row/column diacritics, the
//! image id riding in the foreground colour), so the chip survives redraws,
//! scrolling and multiplexer repaints with no placement bookkeeping, exactly like a
//! full-width image block.
//!
//! Two cells tall on purpose: it is the terminal's version of Slack's 36-pixel chip
//! (a cell is about twice as tall as it is wide, so 4×2 cells is roughly square),
//! and one cell tall would put a face in ~18×19 pixels, which is a smudge. The
//! second row rides in the gutter of the message's first body row, which the layout
//! was already spending on indentation.
//!
//! Terminals that cannot place images keep the initial-on-colour chip. That is the
//! only fallback: the row count is identical either way, so the two skins differ in
//! the gutter and nowhere else.

use ratatui::style::Color;

use crate::tui::gfx::{self, ImageCap, Transmits};

/// Cell footprint of one avatar.
pub const COLS: usize = 4;
pub const ROWS: usize = 2;

/// The bundled portraits (`assets/avatars`, CC0 — see that directory's README).
const PORTRAITS: [&[u8]; 8] = [
    include_bytes!("../../assets/avatars/00.png"),
    include_bytes!("../../assets/avatars/01.png"),
    include_bytes!("../../assets/avatars/02.png"),
    include_bytes!("../../assets/avatars/03.png"),
    include_bytes!("../../assets/avatars/04.png"),
    include_bytes!("../../assets/avatars/05.png"),
    include_bytes!("../../assets/avatars/06.png"),
    include_bytes!("../../assets/avatars/07.png"),
];

/// Which portrait a sender wears. Same hash the colour chip uses, so a member keeps
/// one identity whether or not the terminal can draw pictures.
pub fn index_of(name: &str) -> usize {
    let hash = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    hash as usize % PORTRAITS.len()
}

/// The image key a sender's portrait is transmitted and addressed under. Keyed by
/// portrait rather than by sender: two members sharing a face share one transmit.
fn key_of(name: &str) -> String {
    format!("bingo://avatar/{}", index_of(name))
}

/// One row of a sender's avatar: the placeholder cells plus the colour carrying the
/// image id. `None` past the avatar's own height — the caller pads with spaces.
pub fn placeholder(name: &str, row: usize) -> Option<(String, Color)> {
    if row >= ROWS {
        return None;
    }
    let text = gfx::placeholder_row_text(row, COLS)?;
    let (r, g, b) = gfx::image_id_fg(gfx::image_id_for(&key_of(name)));
    Some((text, Color::Rgb(r, g, b)))
}

/// Transmit payload for every sender in `names` whose portrait the terminal does not
/// hold yet. Empty when there is nothing new to send, so the caller can write it
/// unconditionally.
pub fn transmits(names: &[String], cap: &ImageCap, sent: &mut Transmits) -> Vec<u8> {
    let mut out = Vec::new();
    for name in names {
        let id = gfx::image_id_for(&key_of(name));
        if sent.needs(id) {
            out.extend_from_slice(&gfx::transmit_bytes(
                PORTRAITS[index_of(name)],
                COLS,
                ROWS,
                id,
                cap.transport,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::line::text_width;

    /// The chip's width is load-bearing: the message gutter is sized from it, so a
    /// placeholder row that measured anything but `COLS` would shear every body row
    /// under it.
    #[test]
    fn a_placeholder_row_measures_exactly_the_chip() {
        for row in 0..ROWS {
            let (text, _) = placeholder("scout", row).unwrap_or_else(|| panic!("row {row}"));
            assert_eq!(text_width(&text), COLS, "row {row}: {text:?}");
        }
        assert!(placeholder("scout", ROWS).is_none(), "只有两行");
    }

    /// A sender keeps one face, and the two rows address one image.
    #[test]
    fn rows_share_an_id_and_names_are_stable() {
        let (_, top) = placeholder("scout", 0).unwrap_or_else(|| panic!("row 0"));
        let (_, bottom) = placeholder("scout", 1).unwrap_or_else(|| panic!("row 1"));
        assert_eq!(top, bottom, "同一张图");
        assert_eq!(index_of("scout"), index_of("scout"));
        // Different names generally differ; the set is only eight, so this asserts
        // the mapping is a function of the name, not that it is injective.
        let spread: std::collections::HashSet<usize> =
            ["scout", "qa", "dev", "ui", "main", "user", "docs", "ops"]
                .iter()
                .map(|n| index_of(n))
                .collect();
        assert!(spread.len() >= 4, "八个名字至少落到四张脸: {spread:?}");
    }

    /// Transmit once per portrait, not once per frame or once per sender.
    #[test]
    fn transmits_are_sent_once_per_portrait() {
        let cap = ImageCap::default_cells();
        let mut sent = Transmits::default();
        let names = vec!["scout".to_string(), "qa".to_string()];
        let first = transmits(&names, &cap, &mut sent);
        assert!(!first.is_empty(), "首帧发送");
        assert!(first.starts_with(b"\x1b_G"), "kitty 转义开头");
        assert!(
            transmits(&names, &cap, &mut sent).is_empty(),
            "第二帧不重发"
        );
        // A sender wearing an already-sent face costs nothing either.
        let twin = (0..PORTRAITS.len() * 4)
            .map(|i| format!("m{i}"))
            .find(|n| index_of(n) == index_of("scout"));
        if let Some(twin) = twin {
            assert!(
                transmits(&[twin], &cap, &mut sent).is_empty(),
                "同脸共用一次传输"
            );
        }
    }
}
