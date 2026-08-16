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
///
/// **The first one is main's, and only main's** — see [`MAIN_INDEX`].
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

/// The number of distinct portraits.
pub const COUNT: usize = PORTRAITS.len();

/// The main agent's portrait, reserved (D99).
///
/// @main wears a face like every other participant now that the console has a
/// gutter, and its face has to be *the same one every session* and one no
/// teammate can be handed. Rather than bundle a ninth image, the first portrait
/// is taken out of circulation: [`index_of`] hashes over `1..COUNT` and
/// [`ids`] — the vocabulary `.bingo/team.json` may pin — no longer lists it. So
/// the reservation is total rather than probabilistic, and it costs the crew one
/// face out of eight.
pub const MAIN_INDEX: usize = 0;

/// The portrait ids a blueprint may pin, in order. [`MAIN_INDEX`]'s is not among
/// them: a pinned `main` face would be exactly the collision the reservation
/// exists to prevent.
pub fn ids() -> [&'static str; COUNT - 1] {
    let mut out = [""; COUNT - 1];
    let mut i = MAIN_INDEX + 1;
    while i < COUNT {
        out[i - 1] = PORTRAITS[i].0;
        i += 1;
    }
    out
}

/// A pinned id → its portrait. Unknown ids — and main's, which is not pinnable —
/// fall through to the hash, so a typo in team.json costs a face, not a crash.
pub fn index_of_id(id: &str) -> Option<usize> {
    PORTRAITS
        .iter()
        .position(|(name, _)| *name == id)
        .filter(|index| *index != MAIN_INDEX)
}

/// Which portrait a sender wears when nothing pinned one. Same hash the colour chip
/// uses, so a member keeps one identity whether or not the terminal draws pictures.
///
/// The main agent is answered before the hash and everybody else is hashed over
/// what is left, so no teammate can land on main's face.
pub fn index_of(name: &str) -> usize {
    if name == crate::channels::MAIN_NAME {
        return MAIN_INDEX;
    }
    let hash = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    MAIN_INDEX + 1 + hash as usize % (PORTRAITS.len() - 1)
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
            main_dim: theme.text_secondary,
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
///
/// Public because the gutter is applied by the conversation row builders (D97),
/// which have to take it out of the width *before* wrapping — a body wrapped at
/// the full width and then indented would overrun the terminal by exactly this
/// many cells.
pub fn gutter_width(images: bool) -> usize {
    if images { COLS + 1 } else { GUTTER }
}

fn gutter(images: bool) -> usize {
    gutter_width(images)
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
/// it knows the name (neither main nor the human is a blueprint member), and
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

/// The message gutter of a conversation view (D97): how wide it is, which
/// portrait a sender wears, and the cells that portrait occupies.
///
/// One value threaded through every conversation row builder — @main and the
/// zoomed view's body and live tail — so the surfaces cannot drift on width, on
/// who gets a face, or on which skin the terminal is in.
///
/// **@main has one too, since D99.** A conversation is a conversation: main is a
/// participant like the rest, its portrait is [`MAIN_INDEX`], and the console
/// gets the gutter through this same value rather than through a second
/// convention of its own.
#[derive(Clone, Copy)]
pub struct Gutter<'a> {
    /// Whether the terminal can place images (chip fallback when it cannot).
    pub images: bool,
    pub pal: &'a Palette,
    /// Portraits `.bingo/team.json` pinned, so a crew member keeps one face.
    pub pinned: &'a std::collections::HashMap<String, usize>,
}

impl<'a> Gutter<'a> {
    pub fn new(
        images: bool,
        pal: &'a Palette,
        pinned: &'a std::collections::HashMap<String, usize>,
    ) -> Self {
        Self {
            images,
            pal,
            pinned,
        }
    }

    /// Cells the body has to give up on every row.
    pub fn width(&self) -> usize {
        gutter_width(self.images)
    }

    /// The empty gutter: continuation rows, and every row of a message that is
    /// not the first of its sender's run.
    pub fn blank(&self) -> Line {
        Line::styled(" ".repeat(self.width()), SegStyle::fg(self.pal.main_dim))
    }

    /// The portrait `name` wears, honouring a blueprint pin before the hash —
    /// except main's, which is answered before either (D99): its face is
    /// reserved, and a reservation a pin could override would not be one.
    pub fn index_for(&self, name: &str) -> usize {
        if name == crate::channels::MAIN_NAME {
            return MAIN_INDEX;
        }
        self.pinned
            .get(name)
            .copied()
            .unwrap_or_else(|| index_of(name))
    }

    /// The gutter cells of one message: the face on its own rows, blank below.
    /// `lead` is false for every message after the first of a sender's run —
    /// Slack's convention, and the reason a burst of replies reads as one turn
    /// instead of a column of repeated portraits.
    pub fn cells(&self, index: usize, name: &str, lead: bool) -> Vec<Line> {
        if !lead {
            return Vec::new();
        }
        (0..ROWS)
            .map(|row| gutter_cell(index, name, row, self.images, self.pal))
            .collect()
    }

    /// Indent a message's rows in place. The only entry point the row builders
    /// use, so "avatar on the first row of the run, blank everywhere else" is
    /// stated once.
    pub fn apply(&self, rows: &mut [crate::tui::el::Row], index: usize, name: &str, lead: bool) {
        let cells = self.cells(index, name, lead);
        crate::tui::el::gutter_rows(rows, &cells, &self.blank());
    }
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
        // Different names generally differ; the set is only seven, so this asserts
        // the mapping is a function of the name, not that it is injective.
        let spread: std::collections::HashSet<usize> =
            ["scout", "qa", "dev", "ui", "user", "docs", "ops", "parser"]
                .iter()
                .map(|n| index_of(n))
                .collect();
        assert!(
            spread.len() >= 4,
            "eight names must land on at least four faces: {spread:?}"
        );
    }

    /// Pinning is what makes a crew member's face survive a rename or a reshuffle.
    /// The vocabulary it may pin is every portrait but main's.
    #[test]
    fn ids_pin_a_portrait_and_unknown_ones_fall_through() {
        assert_eq!(ids().len(), COUNT - 1);
        for (offset, id) in ids().into_iter().enumerate() {
            assert_eq!(index_of_id(id), Some(MAIN_INDEX + 1 + offset), "{id}");
        }
        assert_eq!(
            index_of_id("nobody"),
            None,
            "unknown ids are not recognized; fall back to the hash"
        );
    }

    /// @main's face is fixed and nobody else's, which is what lets the console
    /// wear the gutter without a portrait that moves between sessions (D99).
    #[test]
    fn main_keeps_a_face_no_teammate_can_take() {
        let pal = Palette::new(&Theme::dark());
        let mut pinned = std::collections::HashMap::new();
        // Even a blueprint that tries to hand main's portrait to somebody, and
        // even one that tries to repin main itself, does not move it.
        pinned.insert("scout".to_string(), MAIN_INDEX);
        pinned.insert(crate::channels::MAIN_NAME.to_string(), 5);
        let gutter = Gutter::new(false, &pal, &pinned);
        assert_eq!(gutter.index_for(crate::channels::MAIN_NAME), MAIN_INDEX);
        assert_eq!(index_of(crate::channels::MAIN_NAME), MAIN_INDEX);

        // The id of main's portrait is not in the pinnable vocabulary at all, so
        // no team.json can reach it by name.
        let main_id = PORTRAITS[MAIN_INDEX].0;
        assert!(!ids().contains(&main_id), "{main_id} is main's");
        assert_eq!(index_of_id(main_id), None);

        // And the hash never lands there, whatever the name.
        for i in 0..500 {
            let name = format!("agent-{i}");
            assert_ne!(index_of(&name), MAIN_INDEX, "{name} took main's face");
        }
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
