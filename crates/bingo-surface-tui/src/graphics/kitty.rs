//! The kitty graphics protocol, as bytes.
//!
//! One picture reaches the screen in two halves. The bytes of it go out of
//! band, between frames, as an APC sequence (`ESC _ G keys ; payload ESC \`)
//! that both stores the picture and makes a *virtual* placement of it — one
//! that draws nothing by itself. What draws it is the other half: cells of
//! `U+10EEEE` in the frame, each carrying two combining diacritics that say
//! which row and column of the picture it is, with the picture's id in the
//! foreground colour. So the picture rides the terminal's own scrollback:
//! ratatui moves the placeholder cells and the picture follows them.
//!
//! Nothing here knows what a transcript is. It takes an id, some PNG bytes
//! and a rectangle of cells, and answers with bytes.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use base64::Engine;

use super::tmux::{self, Transport};

/// The cell that stands in for a piece of a picture.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// How much base64 one APC sequence carries. The protocol's own limit for a
/// chunked transmission, and a multiple of four so no chunk splits a group.
const CHUNK: usize = 4096;

/// The diacritics that say which row and column of a picture a cell is, in
/// the protocol's own order: index 0 is `U+0305`. Copied from kitty's
/// `gen/rowcolumn-diacritics.txt` (which derives it from Unicode 6.0.0:
/// combining class 230, no decomposition), the first 128 of its 297 — which
/// is more rows and columns than a transcript has room for.
const DIACRITICS: [char; 128] = [
    '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}', '\u{033f}',
    '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}', '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059c}', '\u{059d}', '\u{059e}', '\u{059f}', '\u{05a0}', '\u{05a1}',
    '\u{05a8}', '\u{05a9}', '\u{05ab}', '\u{05ac}', '\u{05af}', '\u{05c4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065a}', '\u{065b}', '\u{065d}', '\u{065e}', '\u{06d6}', '\u{06d7}', '\u{06d8}',
    '\u{06d9}', '\u{06da}', '\u{06db}', '\u{06dc}', '\u{06df}', '\u{06e0}', '\u{06e1}', '\u{06e2}',
    '\u{06e4}', '\u{06e7}', '\u{06e8}', '\u{06eb}', '\u{06ec}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073a}', '\u{073d}', '\u{073f}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074a}', '\u{07eb}', '\u{07ec}', '\u{07ed}', '\u{07ee}',
    '\u{07ef}', '\u{07f0}', '\u{07f1}', '\u{07f3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081b}', '\u{081c}', '\u{081d}', '\u{081e}', '\u{081f}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082a}', '\u{082b}', '\u{082c}',
];

/// The most cells one picture may cover in either direction: past the end of
/// [`DIACRITICS`] there is no way to say which row or column a cell is.
pub const MAX_CELLS: u16 = DIACRITICS.len() as u16;

/// Store this picture and make a virtual placement of it, `cols` by `rows`
/// cells. `q=2` is what keeps the terminal's answer out of the keyboard:
/// every byte it would send back would otherwise be read as typing.
pub fn transmit(id: u32, png: &[u8], cols: u16, rows: u16, transport: Transport) -> Vec<u8> {
    let payload = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = Vec::new();
    let mut chunks = payload.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = chunks.peek().is_some();
        let keys = match first {
            true => opening(id, cols, rows, more),
            false => format!("m={}", u8::from(more)),
        };
        out.extend_from_slice(&apc(&keys, chunk, transport));
        first = false;
    }
    out
}

/// The keys of the first chunk: transmit and display (`a=T`) PNG data
/// (`f=100`) as a virtual placement (`U=1`) of this many cells. Every chunk
/// after it carries `m` alone, as the protocol asks.
fn opening(id: u32, cols: u16, rows: u16, more: bool) -> String {
    let mut keys = format!("a=T,f=100,q=2,U=1,i={id},c={cols},r={rows}");
    if more {
        keys.push_str(",m=1");
    }
    keys
}

/// Forget a picture: its placements and its bytes both (`d=I`), which is the
/// half that gives the terminal its memory back.
pub fn delete(id: u32, transport: Transport) -> Vec<u8> {
    apc(&format!("a=d,d=I,q=2,i={id}"), b"", transport)
}

/// One APC sequence, in whatever envelope reaches the terminal. A chunk is at
/// most [`CHUNK`] bytes of base64 plus its keys, which is inside any length a
/// multiplexer holds, so the envelope is per chunk and no chunk waits on
/// another (M49 brick 1).
fn apc(keys: &str, payload: &[u8], transport: Transport) -> Vec<u8> {
    let mut out = Vec::with_capacity(keys.len() + payload.len() + 6);
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(keys.as_bytes());
    if !payload.is_empty() {
        out.push(b';');
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(b"\x1b\\");
    tmux::wrapped(out, transport)
}

/// One row of a picture, as cells the frame draws: `cols` placeholders, each
/// saying which row and column of the picture it is, all of them carrying the
/// picture's id in the foreground colour.
///
/// Every cell says both numbers rather than leaning on the one before it, so
/// a row the terminal drew only half of still resolves.
pub fn placeholder(id: u32, row: u16, cols: u16) -> Line<'static> {
    let mut text = String::new();
    if let Some(row) = diacritic(row) {
        for column in (0..cols).filter_map(diacritic) {
            text.push(PLACEHOLDER);
            text.push(row);
            text.push(column);
        }
    }
    Line::from(Span::styled(text, colour(id)))
}

/// The picture's id, in the clothes the protocol carries it in: a 24-bit
/// foreground colour. It is a number, not a look — which is why this is the
/// one file outside the token table that names a colour.
fn colour(id: u32) -> Style {
    Style::new().fg(Color::Rgb(
        ((id >> 16) & 0xff) as u8,
        ((id >> 8) & 0xff) as u8,
        (id & 0xff) as u8,
    ))
}

fn diacritic(index: u16) -> Option<char> {
    DIACRITICS.get(usize::from(index)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the protocol's, and these four indexes are the ones the
    /// tests below spell by hand.
    #[test]
    fn the_diacritics_are_the_protocols_own() {
        assert_eq!(diacritic(0), Some('\u{0305}'));
        assert_eq!(diacritic(1), Some('\u{030d}'));
        assert_eq!(diacritic(2), Some('\u{030e}'));
        assert_eq!(diacritic(11), Some('\u{034c}'));
        assert_eq!(diacritic(127), Some('\u{082c}'));
        assert_eq!(diacritic(128), None, "and nothing past the end of it");
        assert_eq!(MAX_CELLS, 128);
    }

    #[test]
    fn one_short_picture_is_one_sequence() {
        assert_eq!(
            transmit(0x0a_0b_0c, b"png!", 4, 2, Transport::Bare),
            b"\x1b_Ga=T,f=100,q=2,U=1,i=658188,c=4,r=2;cG5nIQ==\x1b\\".to_vec()
        );
    }

    /// A payload longer than one chunk: the keys ride the first, every chunk
    /// but the last says `m=1`, and the bytes are the base64 split at 4096.
    #[test]
    fn a_long_picture_is_chunked_at_four_thousand_and_ninety_six() {
        let png = vec![0xabu8; 4096];
        let payload = base64::engine::general_purpose::STANDARD.encode(&png);
        let bytes = transmit(7, &png, 10, 5, Transport::Bare);
        let text = String::from_utf8(bytes).expect("ascii");
        let chunks: Vec<&str> = text.split("\x1b_G").skip(1).collect();
        assert_eq!(chunks.len(), 2, "5464 base64 bytes is two chunks");
        assert!(chunks[0].starts_with("a=T,f=100,q=2,U=1,i=7,c=10,r=5,m=1;"));
        assert!(chunks[1].starts_with("m=0;"));
        let sent: String = chunks
            .iter()
            .filter_map(|chunk| chunk.split_once(';'))
            .map(|(_, rest)| rest.trim_end_matches("\x1b\\"))
            .collect();
        assert_eq!(sent, payload, "and every byte of it goes");
        assert_eq!(
            chunks[0].split_once(';').map(|(_, rest)| rest.len() - 2),
            Some(CHUNK),
            "the first chunk is full"
        );
    }

    #[test]
    fn a_delete_takes_the_bytes_with_it() {
        assert_eq!(
            delete(0xffffff, Transport::Bare),
            b"\x1b_Ga=d,d=I,q=2,i=16777215\x1b\\"
        );
    }

    /// M49 brick 1: under tmux every APC is its own envelope — a two-chunk
    /// picture is two of them, and a delete is one — so tmux is never left
    /// holding half a sequence.
    #[test]
    fn under_tmux_each_chunk_is_its_own_envelope() {
        assert_eq!(
            transmit(0x0a_0b_0c, b"png!", 4, 2, Transport::Tmux),
            b"\x1bPtmux;\x1b\x1b_Ga=T,f=100,q=2,U=1,i=658188,c=4,r=2;cG5nIQ==\x1b\x1b\\\x1b\\"
                .to_vec()
        );
        assert_eq!(
            delete(7, Transport::Tmux),
            b"\x1bPtmux;\x1b\x1b_Ga=d,d=I,q=2,i=7\x1b\x1b\\\x1b\\".to_vec()
        );
        let long = transmit(7, &vec![0xabu8; 4096], 10, 5, Transport::Tmux);
        assert_eq!(
            long.windows(7).filter(|w| *w == b"\x1bPtmux;").count(),
            2,
            "one envelope per chunk, not one around the picture"
        );
    }

    /// One row of cells: the placeholder, its row diacritic, its column
    /// diacritic, and the id as a colour — spelled out here because these
    /// bytes are what a terminal reads a picture off.
    #[test]
    fn a_row_of_cells_says_where_each_of_them_is_and_whose_it_is() {
        let line = placeholder(0x0a_0b_0c, 1, 3);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(
            line.spans[0].content,
            "\u{10eeee}\u{030d}\u{0305}\u{10eeee}\u{030d}\u{030d}\u{10eeee}\u{030d}\u{030e}"
        );
        assert_eq!(
            line.spans[0].style,
            Style::new().fg(Color::Rgb(0x0a, 0x0b, 0x0c))
        );
    }

    /// Each cell is one column wide, whatever it carries: the width the
    /// transcript measures with, and the one ratatui puts it on the screen
    /// with, are the same number.
    #[test]
    fn a_placeholder_cell_is_one_column_wide() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(placeholder(1, 0, 5).spans[0].content.width(), 5);
    }

    /// A picture wider or taller than the table can spell is cut to it
    /// rather than drawn wrong.
    #[test]
    fn no_cell_is_drawn_that_could_not_say_where_it_is() {
        assert_eq!(
            placeholder(1, 0, MAX_CELLS + 4).spans[0]
                .content
                .chars()
                .filter(|c| *c == PLACEHOLDER)
                .count(),
            usize::from(MAX_CELLS)
        );
        assert_eq!(placeholder(1, MAX_CELLS, 4).spans[0].content, "");
    }
}
