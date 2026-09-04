//! tmux, the terminal in front of the terminal.
//!
//! A multiplexer is a terminal of its own: it reads what this surface writes,
//! keeps a screen of its own, and repaints the outer terminal from that. An
//! escape it does not understand it drops, so nothing reaches the terminal a
//! person is looking at unless it is handed over in the one envelope tmux
//! forwards untouched — `DCS tmux; … ST`, with every `ESC` inside doubled
//! (`tmux.1`, `allow-passthrough`).
//!
//! Two things make a picture survive the trip, both read on 2026-09-04
//! (M48 Verified's table): the cells of a virtual placement are ordinary
//! text, so tmux scrolls and repaints them like any other cell, and tmux
//! ≥ 3.4 rewrites the combining characters they carry correctly. kitty's own
//! `icat --passthrough` implies `--unicode-placeholder` for the same reason,
//! and yazi drops direct placement under a multiplexer altogether.
//!
//! GNU screen has no passthrough of this shape and zellij's rules were not
//! researched, so neither of them draws pictures at all (M49 non-goals).

use super::probe::{Named, Version};

/// The tmux that rewrites the placeholder cells' combining characters. Below
/// it the cells arrive mangled, which is a picture drawn wrong rather than a
/// picture not drawn — so it is a floor, like the terminals' own.
const FLOOR: Version = [3, 4, 0];

/// How the bytes of a picture reach the terminal that draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transport {
    /// Straight out: nothing is in the way.
    #[default]
    Bare,
    /// Through tmux's passthrough envelope.
    Tmux,
}

/// How bytes reach the terminal a person is looking at, or `None` when
/// something is in the way that this cannot reach through: GNU screen, whose
/// passthrough is not this one, and anything else calling itself a
/// multiplexer. A terminal that cannot be reached is not asked.
pub fn transport(term: Option<&str>, tmux: bool) -> Option<Transport> {
    if tmux || term.is_some_and(|term| term.starts_with("tmux")) {
        return Some(Transport::Tmux);
    }
    (!crate::terminal::multiplexed(term, tmux)).then_some(Transport::Bare)
}

/// One sequence, in the envelope this transport wants. `Bare` is the sequence
/// itself; tmux wants the `ESC` of every escape inside doubled, or its own
/// parser would end the envelope at the first one.
///
/// The unit is *one* sequence. A picture goes out as several APC chunks and
/// each of them is wrapped on its own, so tmux is never holding half an
/// envelope and no chunk depends on the one before it arriving whole.
pub fn wrapped(sequence: Vec<u8>, transport: Transport) -> Vec<u8> {
    if transport == Transport::Bare {
        return sequence;
    }
    let mut out = b"\x1bPtmux;".to_vec();
    for byte in sequence {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Whether this name is tmux's own. tmux answers XTVERSION itself, with
/// `DCS > | tmux 3.6b ST`, so its name arrives beside the outer terminal's.
pub fn named(terminal: &Named) -> bool {
    terminal.name.eq_ignore_ascii_case("tmux")
}

/// Whether this tmux carries the cells a picture is drawn in ([`FLOOR`]).
pub fn carries_pictures(terminal: &Named) -> bool {
    named(terminal) && terminal.number().is_some_and(|said| said >= FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmux(version: &str) -> Named {
        Named {
            name: "tmux".into(),
            version: version.into(),
        }
    }

    /// tmux is reached through, screen is not reached at all, and a plain
    /// terminal is written to directly.
    #[test]
    fn only_tmux_is_a_multiplexer_this_reaches_through() {
        assert_eq!(
            transport(Some("tmux-256color"), false),
            Some(Transport::Tmux)
        );
        assert_eq!(
            transport(Some("xterm-256color"), true),
            Some(Transport::Tmux),
            "TMUX is set"
        );
        assert_eq!(
            transport(Some("xterm-256color"), false),
            Some(Transport::Bare)
        );
        assert_eq!(transport(None, false), Some(Transport::Bare));
        assert_eq!(
            transport(Some("screen.xterm"), false),
            None,
            "no passthrough"
        );
    }

    /// The envelope, byte for byte, around one APC chunk: `DCS tmux;` then
    /// the chunk with its `ESC`s doubled, then `ST`.
    #[test]
    fn a_chunk_is_wrapped_with_its_escapes_doubled() {
        let chunk = b"\x1b_Ga=T,f=100,q=2,U=1,i=7,c=4,r=2;AAAA\x1b\\".to_vec();
        assert_eq!(
            wrapped(chunk.clone(), Transport::Tmux),
            [
                b"\x1bPtmux;".as_slice(),
                b"\x1b\x1b_Ga=T,f=100,q=2,U=1,i=7,c=4,r=2;AAAA\x1b\x1b\\".as_slice(),
                b"\x1b\\".as_slice(),
            ]
            .concat()
        );
        assert_eq!(wrapped(chunk.clone(), Transport::Bare), chunk);
    }

    /// The combining-character rewrite landed in 3.4; the version a running
    /// tmux gives has a letter on the end (`3.6b`) and reads as far as it is
    /// numbers.
    #[test]
    fn tmux_carries_pictures_from_the_version_that_draws_them_right() {
        assert!(carries_pictures(&tmux("3.4")));
        assert!(carries_pictures(&tmux("3.6b")));
        assert!(!carries_pictures(&tmux("3.3a")));
        assert!(!carries_pictures(&tmux("2.9")));
        assert!(
            !carries_pictures(&tmux("next")),
            "a version nobody can read"
        );
    }

    /// Only tmux is tmux: the outer terminal's own reply is a name beside it,
    /// not a second answer to the same question.
    #[test]
    fn no_other_name_is_tmux() {
        assert!(named(&tmux("3.6b")));
        assert!(!named(&Named {
            name: "ghostty".into(),
            version: "1.3.1".into()
        }));
        assert!(!carries_pictures(&Named {
            name: "kitty".into(),
            version: "9.9.9".into()
        }));
    }
}
