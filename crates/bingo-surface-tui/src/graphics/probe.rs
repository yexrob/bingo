//! The one question this surface asks the terminal about pictures, and how to
//! read the answer.
//!
//! Three queries go out in one write and the answers come back in the order
//! they were asked: the kitty graphics query, which only a terminal that
//! speaks the protocol answers; `CSI 16 t`, which says how many pixels a cell
//! is; and DA1, which *every* terminal answers and which is therefore the end
//! of the read. Nothing is guessed from `TERM` — a name says what a terminal
//! calls itself, not what it can draw.

use super::Cell;

/// What the terminal is asked, in one write. The kitty query carries a one
/// pixel image so a terminal that does not know the protocol has nothing to
/// draw, and `i=31` is the id its answer must name.
///
/// Only a unix terminal is ever asked ([`super::exchange`]), so the question
/// and the predicate that ends its read carry that platform's gate — and the
/// `test` arm keeps both compiled and asserted wherever the suite runs.
#[cfg(any(unix, test))]
pub const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";

/// What came back: whether the terminal speaks kitty, and how big a cell is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub kitty: bool,
    pub cell: Option<Cell>,
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

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answers of six terminals, written from the protocol rather than
    /// captured off six machines: kitty, WezTerm and Ghostty say `OK` and how
    /// big a cell is; iTerm2 and Apple Terminal answer DA1 and nothing else;
    /// a pipe answers nothing at all.
    const KITTY: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;c";
    const WEZTERM: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;36;15t\x1b[?65;4;6;18;22c";
    const GHOSTTY: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;34;17t\x1b[?62;22c";
    const ITERM2: &[u8] = b"\x1b[?62;4c";
    const APPLE_TERMINAL: &[u8] = b"\x1b[?1;2c";
    const NOTHING: &[u8] = b"";

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
            }
        );
    }

    #[test]
    fn a_terminal_that_answers_only_da1_draws_no_pictures() {
        for answer in [ITERM2, APPLE_TERMINAL, NOTHING] {
            assert_eq!(parse(answer), Probe::default(), "{answer:?}");
        }
    }

    #[test]
    fn the_read_ends_on_da1_and_on_nothing_else() {
        assert!(!answered(b"\x1b_Gi=31;OK\x1b\\"), "the graphics answer");
        assert!(!answered(b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t"), "the cell");
        assert!(answered(KITTY));
        assert!(answered(ITERM2), "even with nothing before it");
    }

    /// A keystroke that landed while the terminal was answering is not part
    /// of the answer, and a `c` a person typed is not a DA1 reply.
    #[test]
    fn what_arrives_after_the_da1_reply_is_not_the_answer() {
        let typed = b"\x1b[?62;c\x1b_Gi=31;OK\x1b\\";
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
    fn the_three_queries_go_out_in_one_write() {
        assert_eq!(
            QUERY,
            [
                b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\".as_slice(),
                b"\x1b[16t".as_slice(),
                b"\x1b[c".as_slice(),
            ]
            .concat()
        );
    }
}
