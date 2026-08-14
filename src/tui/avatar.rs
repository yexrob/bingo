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
use crate::tui::line::{Line, SegStyle, text_width};
use crate::tui::theme::Theme;

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

// ---------------------------------------------------------------------------
// The palette and the gutter (moved here when the workspace skin retired, D89)
// ---------------------------------------------------------------------------

/// The colours a sender is drawn in. Accents come from the terminal theme, so a
/// face moves with the rest of the app instead of pinning a second brand on top
/// of it. Foregrounds only: the one background left is the avatar chip, which is
/// a mark that means something rather than chrome.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub badge_bg: Color,
    pub badge_fg: Color,
    pub presence_on: Color,
    pub presence_off: Color,
    pub main_text: Color,
    pub main_dim: Color,
    pub divider: Color,
    pub accent: Color,
    pub warning: Color,
    pub danger: Color,
    pub unread: Color,
    pub avatars: [Color; 6],
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub fn new(theme: &Theme) -> Self {
        let base = Palette {
            badge_bg: theme.claude_deep_strong,
            badge_fg: rgb(0xFFFFFF),
            presence_on: theme.success,
            presence_off: rgb(0x776C62),
            main_text: theme.text,
            main_dim: theme.inactive,
            divider: rgb(0x38332D),
            accent: theme.claude,
            warning: theme.warning,
            danger: theme.error,
            unread: theme.claude_strong,
            avatars: [
                rgb(0x4C9AE0),
                rgb(0x3FA96B),
                rgb(0xC9922E),
                rgb(0xCB5A74),
                rgb(0x7C6BD0),
                rgb(0xC1743C),
            ],
        };
        let pal = if theme.is_dark {
            base
        } else {
            Palette {
                main_text: rgb(0x1D1C1D),
                main_dim: rgb(0x616061),
                divider: rgb(0xDDDDDD),
                accent: theme.claude_deep,
                ..base
            }
        };
        if Theme::terminal_supports_truecolor() {
            pal
        } else {
            pal.downgrade_to_256()
        }
    }

    /// Terminals without 24-bit colour ignore RGB sequences outright, so the
    /// whole palette has to come down to the 256-colour cube together.
    fn downgrade_to_256(self) -> Self {
        let f = crate::tui::theme::to_ansi256;
        Palette {
            badge_bg: f(self.badge_bg),
            badge_fg: f(self.badge_fg),
            presence_on: f(self.presence_on),
            presence_off: f(self.presence_off),
            main_text: f(self.main_text),
            main_dim: f(self.main_dim),
            divider: f(self.divider),
            accent: f(self.accent),
            warning: f(self.warning),
            danger: f(self.danger),
            unread: f(self.unread),
            avatars: self.avatars.map(f),
        }
    }
}

/// Left gutter of a message block when the avatar is a text chip: ` X ` plus one
/// space. With image avatars it is [`COLS`] plus one — see [`gutter`].
const GUTTER: usize = 4;

/// Message gutter: wide enough for whichever avatar the terminal can draw.
fn gutter(images: bool) -> usize {
    if images { COLS + 1 } else { GUTTER }
}

/// Avatar chip for terminals that cannot place images: the sender's initial on a
/// colour, occupying the same gutter the portrait would. The colour is keyed to
/// the same portrait index, so a pinned member keeps one identity in both skins.
fn chip(name: &str, index: usize, pal: &Palette) -> Line {
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "·".to_string());
    let cell = if text_width(&initial) > 1 {
        initial
    } else {
        format!("{initial} ")
    };
    Line::styled(
        format!(" {cell}"),
        SegStyle::fg(pal.badge_fg)
            .with_bg(pal.avatars[index % pal.avatars.len()])
            .bold(),
    )
}

/// The `row`-th gutter cell of a message block: the avatar's own rows when the
/// terminal can place images, blank indentation otherwise. Row 0 rides the name
/// line, row 1 the first body line — which is why the portrait costs no rows the
/// layout was not already spending.
///
/// The portrait index is resolved by the caller: the transcript knows it before
/// it knows the name (neither the hub nor the human is a blueprint member), and
/// a table to look it up in would have been cloned every frame.
pub fn gutter_cell(index: usize, name: &str, row: usize, images: bool, pal: &Palette) -> Line {
    if images {
        if let Some((cells, id)) = placeholder(index, row) {
            let mut line = Line::styled(cells, SegStyle::fg(id));
            line.push_styled(" ", SegStyle::fg(pal.main_text));
            return line;
        }
    } else if row == 0 {
        let mut line = chip(name, index, pal);
        line.push_styled(" ", SegStyle::fg(pal.main_text));
        return line;
    }
    Line::styled(" ".repeat(gutter(images)), SegStyle::fg(pal.main_dim))
}

/// A sender band: the portrait beside the name, as the rows a transcript puts
/// *above* a message rather than beside it. The main chat has no gutter — its
/// bodies run the full width and its `⏺` markers separate prose from tool rows
/// inside one message — so the face goes overhead, where it costs two rows once
/// per message and nothing below it has to move.
///
/// One row where the terminal cannot place images: unlike the workspace gutter,
/// nothing here depends on the two skins having equal height.
pub fn sender_band(
    index: usize,
    name: &str,
    shown: &str,
    images: bool,
    pal: &Palette,
) -> Vec<Line> {
    let mut head = gutter_cell(index, name, 0, images, pal);
    head.push_styled(shown.to_string(), SegStyle::fg(pal.main_text).bold());
    if !images {
        return vec![head];
    }
    vec![head, gutter_cell(index, name, 1, images, pal)]
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
        assert!(placeholder(0, ROWS).is_none(), "only two rows");
    }

    /// A portrait's two rows address one image, and a name maps to one portrait.
    #[test]
    fn rows_share_an_id_and_names_are_stable() {
        let (_, top) = placeholder(3, 0).unwrap_or_else(|| panic!("row 0"));
        let (_, bottom) = placeholder(3, 1).unwrap_or_else(|| panic!("row 1"));
        assert_eq!(top, bottom, "same image");
        assert_eq!(index_of("scout"), index_of("scout"));
        // Different names generally differ; the set is only eight, so this asserts
        // the mapping is a function of the name, not that it is injective.
        let spread: std::collections::HashSet<usize> =
            ["scout", "qa", "dev", "ui", "main", "user", "docs", "ops"]
                .iter()
                .map(|n| index_of(n))
                .collect();
        assert!(
            spread.len() >= 4,
            "eight names must land on at least four faces: {spread:?}"
        );
    }

    /// Pinning is what makes a crew member's face survive a rename or a reshuffle.
    #[test]
    fn ids_pin_a_portrait_and_unknown_ones_fall_through() {
        assert_eq!(ids().len(), COUNT);
        for (i, id) in ids().into_iter().enumerate() {
            assert_eq!(index_of_id(id), Some(i), "{id}");
        }
        assert_eq!(
            index_of_id("nobody"),
            None,
            "unknown ids are not recognized; fall back to the hash"
        );
    }

    /// Transmit once per portrait, not once per frame or once per sender.
    #[test]
    fn transmits_are_sent_once_per_portrait() {
        let cap = ImageCap::default_cells();
        let mut sent = Transmits::default();
        let first = transmits(&[0, 1], &cap, &mut sent);
        assert!(!first.is_empty(), "first frame is sent");
        assert!(first.starts_with(b"\x1b_G"), "kitty escape prefix");
        assert!(
            transmits(&[0, 1], &cap, &mut sent).is_empty(),
            "the second frame is not re-sent"
        );
        // Two senders wearing one face cost one transmit, not two.
        assert!(
            transmits(&[0, 0, 1], &cap, &mut sent).is_empty(),
            "same face shares one transmission"
        );
        assert!(
            !transmits(&[2], &cap, &mut sent).is_empty(),
            "new faces are sent"
        );
    }
}
