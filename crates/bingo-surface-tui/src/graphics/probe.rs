//! The questions this surface asks the terminal about pictures, and how to
//! read the answers.
//!
//! Four queries go out in one write and the answers come back in the order
//! they were asked: the kitty graphics query, which only a terminal that
//! speaks the protocol answers; `CSI 16 t`, which says how many pixels a cell
//! is; XTVERSION, which says what the terminal is; and DA1, which *every*
//! terminal answers and which is therefore the end of the read.
//!
//! The name is asked because the protocol cannot be asked the one thing that
//! matters: whether a `U=1` placeholder cell is drawn. The graphics query
//! answers `OK` either way and no reply anywhere carries a feature bit for it
//! (M48 brick 1), so the name is read and matched against a list of terminals
//! known to draw one ([`super::draws_placeholders`]). Nothing is guessed from
//! `TERM` or from `TERM_PROGRAM`: the first says what a terminal calls itself
//! and the second does not survive `ssh`.

use super::Cell;

/// What the terminal is asked, in one write. The kitty query carries a one
/// pixel image so a terminal that does not know the protocol has nothing to
/// draw, and `i=31` is the id its answer must name.
///
/// XTVERSION (`CSI > 0 q`) goes before DA1, so DA1 is still the last answer
/// and still ends the read.
///
/// Only a unix terminal is ever asked ([`super::exchange`]), so the question
/// and the predicate that ends its read carry that platform's gate — and the
/// `test` arm keeps both compiled and asserted wherever the suite runs.
#[cfg(any(unix, test))]
pub const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[>0q\x1b[c";

/// What came back: whether the terminal speaks kitty, how big a cell is, and
/// what the terminal says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub kitty: bool,
    pub cell: Option<Cell>,
    pub terminal: Option<Named>,
}

/// What a terminal calls itself, as XTVERSION spells it: `kitty(0.46.2)` and
/// `ghostty 1.3.1` are the two shapes in the wild, so both are read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub version: String,
}

/// Whether the read is over. DA1 was asked last and is answered by every
/// terminal there is, so its reply ends the read — a probe that waited for
/// its clock instead would cost every start-up the whole timeout.
#[cfg(any(unix, test))]
pub fn answered(so_far: &[u8]) -> bool {
    da1(so_far).is_some()
}

/// What the answer says. Anything after the DA1 reply is somebody else's —
/// a keystroke that arrived while the terminal was answering — so the read
/// stops there.
pub fn parse(answer: &[u8]) -> Probe {
    let answer = &answer[..da1(answer).unwrap_or(answer.len())];
    Probe {
        kitty: says_ok(answer),
        cell: cell(answer),
        terminal: named(answer),
    }
}

/// Where the DA1 reply (`CSI ? … c`) ends, when it has landed.
fn da1(bytes: &[u8]) -> Option<usize> {
    let start = find(bytes, b"\x1b[?")?;
    let end = bytes[start..].iter().position(|b| *b == b'c')?;
    Some(start + end + 1)
}

/// Whether the graphics query came back `OK` for the id it was asked under.
/// A terminal that answers something else about `i=31` — a failure code —
/// answers `no`, which is what fail-closed means here.
fn says_ok(bytes: &[u8]) -> bool {
    let Some(body) = apc(bytes) else {
        return false;
    };
    match body.split_once(';') {
        Some((keys, answer)) => keys.contains("i=31") && answer.starts_with("OK"),
        None => false,
    }
}

/// The body of the first APC block (`ESC _ … ESC \`), as text.
fn apc(bytes: &[u8]) -> Option<&str> {
    let start = find(bytes, b"\x1b_G")? + 3;
    let end = start + find(&bytes[start..], b"\x1b\\")?;
    std::str::from_utf8(&bytes[start..end]).ok()
}

/// The cell in pixels, from `CSI 6 ; height ; width t`. Height leads, which
/// is the one thing about this reply worth writing down twice.
fn cell(bytes: &[u8]) -> Option<Cell> {
    let start = find(bytes, b"\x1b[6;")? + 4;
    let end = start + bytes[start..].iter().position(|b| *b == b't')?;
    let reply = std::str::from_utf8(&bytes[start..end]).ok()?;
    let (height, width) = reply.split_once(';')?;
    let cell = Cell {
        height: height.parse().ok()?,
        width: width.parse().ok()?,
    };
    (cell.width > 0 && cell.height > 0).then_some(cell)
}

/// What the terminal called itself, from XTVERSION's `DCS > | text ST`. A
/// reply with no version at all is no answer: a name alone cannot be held to
/// a floor, and this list is a list of floors.
fn named(bytes: &[u8]) -> Option<Named> {
    let text = dcs(bytes)?.trim();
    let (name, version) = split(text)?;
    (!name.is_empty() && !version.is_empty()).then(|| Named {
        name: name.to_string(),
        version: version.to_string(),
    })
}

/// The two shapes a reply comes in: `name(version)` and `name version`.
fn split(text: &str) -> Option<(&str, &str)> {
    match text.strip_suffix(')') {
        Some(inside) => inside.split_once('('),
        None => text.split_once(' '),
    }
    .map(|(name, version)| (name.trim(), version.trim()))
}

/// The body of the first XTVERSION block (`ESC P > | … ESC \`), as text.
fn dcs(bytes: &[u8]) -> Option<&str> {
    let start = find(bytes, b"\x1bP>|")? + 4;
    let end = start + find(&bytes[start..], b"\x1b\\")?;
    std::str::from_utf8(&bytes[start..end]).ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answers of eight terminals, written from the protocol and from the
    /// version strings each of them is documented to send, rather than
    /// captured off eight machines. kitty, Ghostty, WezTerm and Konsole all
    /// say `OK` and how big a cell is — only the first two draw the
    /// placeholder cells, which is what [`super::super::draws_placeholders`]
    /// is for; iTerm2 here is an old one that answers DA1 only; foot names
    /// itself and speaks no graphics protocol; Apple Terminal names nothing;
    /// a pipe answers nothing at all.
    const KITTY: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1bP>|kitty(0.46.2)\x1b\\\x1b[?62;c";
    const GHOSTTY: &[u8] =
        b"\x1b_Gi=31;OK\x1b\\\x1b[6;34;17t\x1bP>|ghostty 1.3.1\x1b\\\x1b[?62;22c";
    const WEZTERM: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;36;15t\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\x1b[?65;4;6;18;22c";
    const KONSOLE: &[u8] =
        b"\x1b_Gi=31;OK\x1b\\\x1b[6;30;14t\x1bP>|Konsole 26.08.0\x1b\\\x1b[?62;c";
    const FOOT: &[u8] = b"\x1b[6;25;12t\x1bP>|foot(1.28.0)\x1b\\\x1b[?62;4c";
    const ITERM2: &[u8] = b"\x1b[?62;4c";
    const APPLE_TERMINAL: &[u8] = b"\x1b[?1;2c";
    const NOTHING: &[u8] = b"";

    fn named(name: &str, version: &str) -> Option<Named> {
        Some(Named {
            name: name.into(),
            version: version.into(),
        })
    }

    #[test]
    fn a_terminal_that_draws_pictures_says_ok_and_how_big_a_cell_is() {
        assert_eq!(
            parse(KITTY),
            Probe {
                kitty: true,
                cell: Some(Cell {
                    width: 10,
                    height: 20
                }),
                terminal: named("kitty", "0.46.2"),
            }
        );
        assert_eq!(
            parse(GHOSTTY),
            Probe {
                kitty: true,
                cell: Some(Cell {
                    width: 17,
                    height: 34
                }),
                terminal: named("ghostty", "1.3.1"),
            }
        );
        assert_eq!(
            parse(WEZTERM),
            Probe {
                kitty: true,
                cell: Some(Cell {
                    width: 15,
                    height: 36
                }),
                terminal: named("WezTerm", "20240203-110809-5046fc22"),
            }
        );
    }

    #[test]
    fn a_terminal_that_answers_only_da1_draws_no_pictures() {
        for answer in [ITERM2, APPLE_TERMINAL, NOTHING] {
            assert_eq!(parse(answer), Probe::default(), "{answer:?}");
        }
    }

    /// A terminal may name itself and speak no graphics protocol at all.
    #[test]
    fn a_name_is_read_whether_or_not_there_are_pictures_behind_it() {
        let foot = parse(FOOT);
        assert!(!foot.kitty);
        assert_eq!(foot.terminal, named("foot", "1.28.0"));
        assert_eq!(parse(KONSOLE).terminal, named("Konsole", "26.08.0"));
    }

    /// The two shapes of the reply, and the ones that say nothing usable: a
    /// name with no version cannot be held to a floor, so it is no answer.
    #[test]
    fn the_name_is_read_from_either_shape_and_from_nothing_else() {
        let read = |reply: &str| {
            let bytes = format!("\x1bP>|{reply}\x1b\\\x1b[?62;c");
            parse(bytes.as_bytes()).terminal
        };
        assert_eq!(read("iTerm2 3.6.11"), named("iTerm2", "3.6.11"));
        assert_eq!(read("Rio 0.5.27"), named("Rio", "0.5.27"));
        assert_eq!(read(" kitty(0.28.0) "), named("kitty", "0.28.0"));
        assert_eq!(read("kitty"), None, "a name with no version");
        assert_eq!(read("kitty()"), None, "and an empty one");
        assert_eq!(read(""), None);
        assert_eq!(
            parse(b"\x1bP>|kitty(0.46.2)\x1b[?62;c").terminal,
            None,
            "an unterminated reply is not an answer"
        );
    }

    #[test]
    fn the_read_ends_on_da1_and_on_nothing_else() {
        assert!(!answered(b"\x1b_Gi=31;OK\x1b\\"), "the graphics answer");
        assert!(!answered(b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t"), "the cell");
        assert!(
            !answered(b"\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.46.2)\x1b\\"),
            "the name"
        );
        assert!(answered(KITTY));
        assert!(answered(ITERM2), "even with nothing before it");
    }

    /// A keystroke that landed while the terminal was answering is not part
    /// of the answer, and a `c` a person typed is not a DA1 reply.
    #[test]
    fn what_arrives_after_the_da1_reply_is_not_the_answer() {
        let typed = b"\x1b[?62;c\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.46.2)\x1b\\";
        assert_eq!(parse(typed), Probe::default());
        assert!(!answered(b"cc"), "a typed c is not the reply");
    }

    /// Fail closed: an answer about another image, a failure code, and a
    /// half-written reply all mean no pictures.
    #[test]
    fn an_answer_that_is_not_ok_for_this_query_is_a_no() {
        assert!(!parse(b"\x1b_Gi=99;OK\x1b\\\x1b[?62;c").kitty, "another id");
        assert!(
            !parse(b"\x1b_Gi=31;ENOTSUPPORTED:x\x1b\\\x1b[?62;c").kitty,
            "a refusal"
        );
        assert!(!parse(b"\x1b_Gi=31;OK\x1b[?62;c").kitty, "unterminated");
    }

    /// A cell reply that says a cell is nothing is no answer at all: the
    /// graphics stay off rather than guess 8×16 (M46 risk 2).
    #[test]
    fn a_cell_of_no_pixels_is_no_cell() {
        assert_eq!(cell(b"\x1b[6;0;10t"), None);
        assert_eq!(cell(b"\x1b[6;20;0t"), None);
        assert_eq!(cell(b"\x1b[6;20t"), None, "one number is not two");
        assert_eq!(cell(b"\x1b[6;a;bt"), None);
    }

    /// The bytes that go out, spelled once here and asserted so a rewrite of
    /// the sequence is a decision and not a typo.
    #[test]
    fn the_four_queries_go_out_in_one_write() {
        assert_eq!(
            QUERY,
            [
                b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\".as_slice(),
                b"\x1b[16t".as_slice(),
                b"\x1b[>0q".as_slice(),
                b"\x1b[c".as_slice(),
            ]
            .concat()
        );
    }
}
