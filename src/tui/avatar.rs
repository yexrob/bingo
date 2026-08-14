//! Sender avatars: bundled portraits placed as kitty images.
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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::tui::gfx::{self, ImageCap, Transmits};

/// Cell footprint of one avatar.
pub const COLS: usize = 4;
pub const ROWS: usize = 2;

/// The bundled portraits (`assets/avatars`, CC0 — see that directory's README).
/// The name is the id `.bingo/team.json` pins with, so a crew member keeps one face
/// across sessions instead of whatever a hash of its instance name lands on.
const PORTRAITS: [(&str, &[u8]); 20] = [
    ("emi", include_bytes!("../../assets/avatars/emi.png")),
    ("kenji", include_bytes!("../../assets/avatars/kenji.png")),
    ("sora", include_bytes!("../../assets/avatars/sora.png")),
    ("mika", include_bytes!("../../assets/avatars/mika.png")),
    ("taro", include_bytes!("../../assets/avatars/taro.png")),
    ("jin", include_bytes!("../../assets/avatars/jin.png")),
    ("kai", include_bytes!("../../assets/avatars/kai.png")),
    ("rio", include_bytes!("../../assets/avatars/rio.png")),
    (
        "identicon-01",
        include_bytes!("../../assets/avatars/identicon-01.png"),
    ),
    (
        "identicon-02",
        include_bytes!("../../assets/avatars/identicon-02.png"),
    ),
    (
        "identicon-03",
        include_bytes!("../../assets/avatars/identicon-03.png"),
    ),
    (
        "identicon-04",
        include_bytes!("../../assets/avatars/identicon-04.png"),
    ),
    (
        "identicon-05",
        include_bytes!("../../assets/avatars/identicon-05.png"),
    ),
    (
        "identicon-06",
        include_bytes!("../../assets/avatars/identicon-06.png"),
    ),
    (
        "identicon-07",
        include_bytes!("../../assets/avatars/identicon-07.png"),
    ),
    (
        "identicon-08",
        include_bytes!("../../assets/avatars/identicon-08.png"),
    ),
    (
        "identicon-09",
        include_bytes!("../../assets/avatars/identicon-09.png"),
    ),
    (
        "identicon-10",
        include_bytes!("../../assets/avatars/identicon-10.png"),
    ),
    (
        "identicon-11",
        include_bytes!("../../assets/avatars/identicon-11.png"),
    ),
    (
        "identicon-12",
        include_bytes!("../../assets/avatars/identicon-12.png"),
    ),
];

const DEFAULT_IDS: [&str; 12] = [
    "identicon-01",
    "identicon-02",
    "identicon-03",
    "identicon-04",
    "identicon-05",
    "identicon-06",
    "identicon-07",
    "identicon-08",
    "identicon-09",
    "identicon-10",
    "identicon-11",
    "identicon-12",
];
const DEFAULT_OFFSET: usize = PORTRAITS.len() - DEFAULT_IDS.len();
static DEFAULT_NONCE: AtomicU64 = AtomicU64::new(0);

/// Every portrait id, in order — the vocabulary a blueprint may pin.
pub fn ids() -> [&'static str; COUNT] {
    PORTRAITS.map(|(id, _)| id)
}

/// The number of distinct portraits, so a roster can hand out different ones.
pub const COUNT: usize = PORTRAITS.len();
pub const DEFAULT_COUNT: usize = DEFAULT_IDS.len();

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
    DEFAULT_OFFSET + hash as usize % DEFAULT_COUNT
}

/// Choose one geometric avatar once, preferring an id not already used by the team.
/// The choice is persisted by the caller; the clock and nonce only spread new picks.
pub fn random_default_id<'a>(taken: impl IntoIterator<Item = &'a str>, seed: &str) -> &'static str {
    let mut reserved = [false; DEFAULT_COUNT];
    for id in [
        PORTRAITS[index_of(crate::channels::HUB_NAME)].0,
        PORTRAITS[index_of(crate::channels::USER_NAME)].0,
    ] {
        if let Some(index) = DEFAULT_IDS.iter().position(|candidate| *candidate == id) {
            reserved[index] = true;
        }
    }
    let mut unavailable = [false; DEFAULT_COUNT];
    unavailable.copy_from_slice(&reserved);
    for id in taken {
        if let Some(index) = DEFAULT_IDS.iter().position(|candidate| *candidate == id) {
            unavailable[index] = true;
        }
    }
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    let counter = DEFAULT_NONCE.fetch_add(1, Ordering::Relaxed);
    let hash = seed
        .bytes()
        .fold(clock ^ counter.rotate_left(17), |value, byte| {
            value
                .wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(byte))
        });
    let start = hash as usize % DEFAULT_COUNT;
    for offset in 0..DEFAULT_COUNT {
        let index = (start + offset) % DEFAULT_COUNT;
        if !unavailable[index] {
            return DEFAULT_IDS[index];
        }
    }
    for offset in 0..DEFAULT_COUNT {
        let index = (start + offset) % DEFAULT_COUNT;
        if !reserved[index] {
            return DEFAULT_IDS[index];
        }
    }
    unreachable!("the automatic avatar pool must contain an unreserved portrait")
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
        assert!(placeholder(0, ROWS).is_none(), "only two rows");
    }

    /// A portrait's two rows address one image, and a name maps to one portrait.
    #[test]
    fn rows_share_an_id_and_names_are_stable() {
        let (_, top) = placeholder(3, 0).unwrap_or_else(|| panic!("row 0"));
        let (_, bottom) = placeholder(3, 1).unwrap_or_else(|| panic!("row 1"));
        assert_eq!(top, bottom, "same image");
        assert_eq!(index_of("scout"), index_of("scout"));
        // Different names generally differ; this asserts
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

    #[test]
    fn legacy_ids_keep_their_order_and_all_bundled_portraits_decode() {
        let all = ids();
        assert_eq!(
            &all[..8],
            &["emi", "kenji", "sora", "mika", "taro", "jin", "kai", "rio"]
        );
        for (id, bytes) in PORTRAITS {
            let portrait = image::load_from_memory(bytes)
                .unwrap_or_else(|error| panic!("{id} must decode: {error}"));
            if id.starts_with("identicon-") {
                assert_eq!((portrait.width(), portrait.height()), (256, 256), "{id}");
            }
        }
    }

    #[test]
    fn automatic_faces_use_only_the_geometric_pool() {
        for name in ["main", "user", "lead", "reviewer", "开发者"] {
            assert!(index_of(name) >= DEFAULT_OFFSET);
            assert!(DEFAULT_IDS.contains(&PORTRAITS[index_of(name)].0));
        }
        let taken = DEFAULT_IDS[..DEFAULT_COUNT - 1].iter().copied();
        assert_eq!(
            random_default_id(taken, "last-slot"),
            DEFAULT_IDS[DEFAULT_COUNT - 1]
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
