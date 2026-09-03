//! Which terminals draw a `U=1` placeholder cell, by name and version.
//!
//! This is a list because it cannot be a question. The kitty graphics
//! protocol's own query (`a=q`) answers `OK` or an error and carries no
//! feature bits, and neither DA1 nor XTVERSION has a field for placeholders
//! (spec and replies read 2026-09-04). Asking by side effect is worse than
//! not asking: WezTerm and Konsole parse the `U` key, ignore it, and draw a
//! real placement at the cursor, so a probe would leave a picture stuck to
//! the screen on exactly the terminals it is trying to rule out.
//!
//! So a terminal draws pictures here only if it says `OK`, says how big a
//! cell is, *and* names itself as one of these four. Silence is not a yes.

use super::probe::Named;

/// The terminals known to draw placeholder cells, and the version each of
/// them learned to. Case is not part of a name.
///
/// Before adding a fifth, read its parser rather than its changelog: the two
/// that get this wrong both *store* the `U` key and never read it. The four
/// here, with what was read on 2026-09-04:
///
/// - **kitty 0.28.0** — the protocol's own "Unicode placeholders" section is
///   marked `versionadded: 0.28.0` (PR kovidgoyal/kitty#5664).
/// - **Ghostty 1.0.0** — ghostty-org/ghostty#2015, merged 2024-07-31, before
///   the 1.0 release.
/// - **iTerm2 3.5.6** — gnachman/iTerm2 commit 4fe5b21 (2024-08-21), first in
///   the 3.5.6 stable of 2024-11-02.
/// - **Rio 0.5.27** — raphamorim/rio#1893 (2026-08-24), fixing #1891.
///
/// And the ones that answer `OK` and get the chip instead: WezTerm
/// (wezterm#986, open since 2021-07-28), Konsole (`Vt102Emulation.cpp` stores
/// `U` and never reads it; bugs.kde.org 523718), Warp (warp#6210, rejects the
/// placement silently under `q=2`), VS Code's xterm.js (xterm.js#5711).
const FLOORS: [(&str, Version); 4] = [
    ("kitty", [0, 28, 0]),
    ("ghostty", [1, 0, 0]),
    ("iterm2", [3, 5, 6]),
    ("rio", [0, 5, 27]),
];

/// Three numbers, which is as much of a version as any of these floors needs.
type Version = [u32; 3];

/// Whether this terminal draws the placeholder cells a virtual placement is
/// made of. A name that is not on the list, and a version below its floor,
/// both answer no — as does a version that will not parse, since a terminal
/// that cannot say which one it is cannot be held to a floor.
pub fn draws_placeholders(terminal: &Named) -> bool {
    FLOORS.iter().any(|(name, floor)| {
        terminal.name.eq_ignore_ascii_case(name)
            && version(&terminal.version).is_some_and(|said| said >= *floor)
    })
}

/// The dotted numbers a version starts with: `0.46.2` is `[0, 46, 2]` and
/// `1.2` is `[1, 2, 0]`. A part that does not begin with a digit ends the
/// reading, and one at the front means there is no version here at all —
/// Warp's `v0.2026…` is a string, not a number. Only the four names above
/// ever reach a floor, so a date read as a very large number (WezTerm's
/// `20240203-…`) says nothing about anything.
fn version(text: &str) -> Option<Version> {
    let mut parts = text.split('.');
    let mut read = [0u32; 3];
    read[0] = number(parts.next()?)?;
    for slot in read.iter_mut().skip(1) {
        match parts.next().and_then(number) {
            Some(n) => *slot = n,
            None => break,
        }
    }
    Some(read)
}

/// The digits a part begins with: `28` of `28`, `1` of `1-beta`, and nothing
/// at all of `v0`.
fn number(part: &str) -> Option<u32> {
    let digits = part.len() - part.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    part.get(..digits)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draws(name: &str, version: &str) -> bool {
        draws_placeholders(&Named {
            name: name.into(),
            version: version.into(),
        })
    }

    /// The four, each at its floor and each one release below it.
    #[test]
    fn the_four_draw_placeholders_from_the_version_that_learned_to() {
        for (name, at, below) in [
            ("kitty", "0.28.0", "0.27.9"),
            ("ghostty", "1.0.0", "0.9.9"),
            ("iTerm2", "3.5.6", "3.5.5"),
            ("Rio", "0.5.27", "0.5.26"),
        ] {
            assert!(draws(name, at), "{name} {at}");
            assert!(!draws(name, below), "{name} {below}");
        }
    }

    /// And each of them well past its floor, in the spelling its own
    /// XTVERSION reply uses.
    #[test]
    fn a_terminal_past_its_floor_still_draws() {
        assert!(draws("kitty", "0.46.2"));
        assert!(draws("ghostty", "1.3.1"));
        assert!(draws("iTerm2", "3.6.11"));
        assert!(draws("Rio", "0.6.0"));
        assert!(draws("KITTY", "0.46.2"), "a name is not case");
    }

    /// The four that answer `OK` and draw tofu. These are the whole reason
    /// the list exists.
    #[test]
    fn a_terminal_that_says_ok_and_draws_no_placeholder_is_not_on_the_list() {
        assert!(!draws("WezTerm", "20240203-110809-5046fc22"));
        assert!(!draws("Konsole", "26.08.0"));
        assert!(!draws("Warp", "v0.2026.06.10.08.11.stable_02"));
        assert!(!draws("xterm.js", "5.5.0"));
        assert!(!draws("foot", "1.28.0"), "nor one with no graphics at all");
    }

    /// A version nobody can compare is below every floor: silence is not a
    /// yes, and neither is noise.
    #[test]
    fn a_version_that_will_not_parse_is_below_every_floor() {
        for said in ["", "unknown", "v1.0.0", ".", "x.28.0"] {
            assert!(!draws("kitty", said), "{said:?}");
        }
    }

    /// Two numbers, and a suffix on the third: read as far as it reads.
    #[test]
    fn a_version_is_read_as_far_as_it_is_numbers() {
        assert_eq!(version("1.2"), Some([1, 2, 0]));
        assert_eq!(version("0.28.0-beta.3"), Some([0, 28, 0]));
        assert_eq!(version("2"), Some([2, 0, 0]));
        assert_eq!(version("1.x.3"), Some([1, 0, 0]));
        assert_eq!(version("20240203-110809-5046fc22"), Some([20240203, 0, 0]));
        assert_eq!(version("v0.2026.06"), None);
    }
}
