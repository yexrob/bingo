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
/// The name is the id `.bingo/team.json` pins with, so a crew member keeps one face
/// across sessions instead of whatever a hash of its instance name lands on.
const PORTRAITS: [(&str, &[u8]); 8] = [
    ("emi", include_bytes!("../../assets/avatars/emi.png")),
    ("kenji", include_bytes!("../../assets/avatars/kenji.png")),
    ("sora", include_bytes!("../../assets/avatars/sora.png")),
    ("mika", include_bytes!("../../assets/avatars/mika.png")),
    ("taro", include_bytes!("../../assets/avatars/taro.png")),
    ("jin", include_bytes!("../../assets/avatars/jin.png")),
    ("kai", include_bytes!("../../assets/avatars/kai.png")),
    ("rio", include_bytes!("../../assets/avatars/rio.png")),
];

/// Every portrait id, in order — the vocabulary a blueprint may pin.
pub fn ids() -> [&'static str; COUNT] {
    PORTRAITS.map(|(id, _)| id)
}

/// The number of distinct portraits, so a roster can hand out different ones.
pub const COUNT: usize = PORTRAITS.len();

/// A pinned id → its portrait. Unknown ids fall through to the hash, so a typo in
/// team.json costs a face, not a crash.
pub fn index_of_id(id: &str) -> Option<usize> {
    PORTRAITS.iter().position(|(name, _)| *name == id)
}

/// Which portrait a sender wears when nothing pinned one. Same hash the colour chip
/// uses, so a member keeps one identity whether or not the terminal draws pictures.
pub fn index_of(name: &str) -> usize {
    let hash = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    hash as usize % PORTRAITS.len()
}

/// The image key a portrait is transmitted and addressed under. Keyed by portrait
/// rather than by sender: two members sharing a face share one transmit.
fn key_of(index: usize) -> String {
    format!("bingo://avatar/{}", index % PORTRAITS.len())
}

/// One row of a portrait: the placeholder cells plus the colour carrying the image
/// id. `None` past the avatar's own height — the caller pads with spaces.
pub fn placeholder(index: usize, row: usize) -> Option<(String, Color)> {
    if row >= ROWS {
        return None;
    }
    let text = gfx::placeholder_row_text(row, COLS)?;
    let (r, g, b) = gfx::image_id_fg(gfx::image_id_for(&key_of(index)));
    Some((text, Color::Rgb(r, g, b)))
}

/// Transmit payload for every portrait in `indices` the terminal does not hold yet.
/// Empty when there is nothing new to send, so the caller can write it
/// unconditionally.
pub fn transmits(indices: &[usize], cap: &ImageCap, sent: &mut Transmits) -> Vec<u8> {
    let mut out = Vec::new();
    for index in indices {
        let index = index % PORTRAITS.len();
        let id = gfx::image_id_for(&key_of(index));
        if sent.needs(id) {
            out.extend_from_slice(&gfx::transmit_bytes(
                PORTRAITS[index].1,
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
            let (text, _) = placeholder(0, row).unwrap_or_else(|| panic!("row {row}"));
            assert_eq!(text_width(&text), COLS, "row {row}: {text:?}");
        }
        assert!(placeholder(0, ROWS).is_none(), "只有两行");
    }

    /// A portrait's two rows address one image, and a name maps to one portrait.
    #[test]
    fn rows_share_an_id_and_names_are_stable() {
        let (_, top) = placeholder(3, 0).unwrap_or_else(|| panic!("row 0"));
        let (_, bottom) = placeholder(3, 1).unwrap_or_else(|| panic!("row 1"));
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

    /// Pinning is what makes a crew member's face survive a rename or a reshuffle.
    #[test]
    fn ids_pin_a_portrait_and_unknown_ones_fall_through() {
        assert_eq!(ids().len(), COUNT);
        for (i, id) in ids().into_iter().enumerate() {
            assert_eq!(index_of_id(id), Some(i), "{id}");
        }
        assert_eq!(index_of_id("nobody"), None, "未知 id 不认，交给哈希兜底");
    }

    /// Transmit once per portrait, not once per frame or once per sender.
    #[test]
    fn transmits_are_sent_once_per_portrait() {
        let cap = ImageCap::default_cells();
        let mut sent = Transmits::default();
        let first = transmits(&[0, 1], &cap, &mut sent);
        assert!(!first.is_empty(), "首帧发送");
        assert!(first.starts_with(b"\x1b_G"), "kitty 转义开头");
        assert!(
            transmits(&[0, 1], &cap, &mut sent).is_empty(),
            "第二帧不重发"
        );
        // Two senders wearing one face cost one transmit, not two.
        assert!(
            transmits(&[0, 0, 1], &cap, &mut sent).is_empty(),
            "同脸共用一次传输"
        );
        assert!(!transmits(&[2], &cap, &mut sent).is_empty(), "新面孔照发");
    }
}
