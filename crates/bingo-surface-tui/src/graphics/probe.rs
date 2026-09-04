//! The questions this surface asks the terminal about pictures, and how to
//! read the answers.
//!
//! Four queries go out in one write and the answers come back in the order
//! they were asked: the kitty graphics query, which only a terminal that
//! speaks the protocol answers; `CSI 16 t`, which says how many pixels a cell
//! is; XTVERSION, which says what the terminal is; and DA1, which *every*
//! terminal answers and which is therefore the end of the read.
//!
//! Under tmux the same four go inside its passthrough envelope, so it is the
//! terminal in front of tmux that answers them ([`query`]); one more
//! XTVERSION goes out bare, which is tmux answering for itself.
//!
//! The name is asked because the protocol cannot be asked the one thing that
//! matters: whether a `U=1` placeholder cell is drawn. The graphics query
//! answers `OK` either way and no reply anywhere carries a feature bit for it
//! (M48 brick 1), so the name is read and matched against a list of terminals
//! known to draw one ([`super::draws_placeholders`]). Nothing is guessed from
//! `TERM` or from `TERM_PROGRAM`: the first says what a terminal calls itself
//! and the second does not survive `ssh`.

use super::Cell;
use super::tmux::{self, Transport};

/// The four questions, in one write. The kitty query carries a one pixel
/// image so a terminal that does not know the protocol has nothing to draw,
/// and `i=31` is the id its answer must name.
///
/// XTVERSION (`CSI > 0 q`) goes before DA1, so DA1 is still the last answer
/// and still ends the read.
#[cfg(any(unix, test))]
const ASKED: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[>0q\x1b[c";

/// XTVERSION on its own, for the multiplexer in front.
#[cfg(any(unix, test))]
const XTVERSION: &[u8] = b"\x1b[>0q";

/// What the terminal is asked, as this transport has to carry it.
///
/// Under tmux all four questions travel in one passthrough envelope, so the
/// *outer* terminal answers all four and its DA1 reply is still what ends the
/// read. One unwrapped question goes ahead of them, and only one: tmux
/// answers XTVERSION itself, and it answers at once — `input_reply` writes
/// straight back to the pane while a passthrough only reaches the outer
/// terminal on the next flush (tmux `input.c`, read 2026-09-04) — so tmux's
/// own name arrives first and the outer terminal's four answers follow it.
///
/// Nothing else may go out unwrapped. tmux answers DA1 and `CSI 16 t` itself
/// too (`input.c` `case 16:` replies `CSI 6 ; ypixel ; xpixel t`), and an
/// unwrapped DA1 would come back before the outer terminal had said anything
/// at all — ending the read on an answer with nothing in it.
///
/// Only a unix terminal is ever asked ([`super::exchange`]), so the question
/// and the predicate that ends its read carry that platform's gate — and the
/// `test` arm keeps both compiled and asserted wherever the suite runs.
#[cfg(any(unix, test))]
pub fn query(transport: Transport) -> Vec<u8> {
    match transport {
        Transport::Bare => ASKED.to_vec(),
        Transport::Tmux => [
            XTVERSION,
            tmux::wrapped(ASKED.to_vec(), transport).as_slice(),
        ]
        .concat(),
    }
}

/// What came back: whether the terminal speaks kitty, how big a cell is, and
/// what named itself. There is more than one name under a multiplexer — tmux
/// answers for itself and the outer terminal answers through it — so they are
/// all kept, in the order they arrived.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub kitty: bool,
    pub cell: Option<Cell>,
    pub terminals: Vec<Named>,
}

impl Probe {
    /// One answer on top of another: a reply that came in after the read
    /// ended is still a reply, and what it says joins what the read heard
    /// ([`crate::late`], M60 brick 2). Nothing is ever unsaid — a terminal
    /// that answered `OK` does not stop having answered — so merging is
    /// monotone, and a second late reply that says nothing new changes
    /// nothing at all.
    ///
    /// A name already in the list is not added twice: the same terminal
    /// answering again is the same terminal.
    pub fn and(mut self, later: Probe) -> Probe {
        self.kitty |= later.kitty;
        self.cell = self.cell.or(later.cell);
        for named in later.terminals {
            if !self.terminals.contains(&named) {
                self.terminals.push(named);
            }
        }
        self
    }
}

/// What a terminal calls itself, as XTVERSION spells it: `kitty(0.46.2)` and
/// `ghostty 1.3.1` are the two shapes in the wild, so both are read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub version: String,
}

/// Three numbers, which is as much of a version as any floor here needs.
pub type Version = [u32; 3];

impl Named {
    /// The dotted numbers this version starts with: `0.46.2` is `[0, 46, 2]`
    /// and `1.2` is `[1, 2, 0]`. A part that does not begin with a digit ends
    /// the reading, and one at the front means there is no version here at
    /// all — Warp's `v0.2026…` is a string, not a number. Only a name already
    /// on a list is ever held to a floor, so a date read as a very large
    /// number (WezTerm's `20240203-…`) says nothing about anything.
    pub fn number(&self) -> Option<Version> {
        let mut parts = self.version.split('.');
        let mut read = [0u32; 3];
        read[0] = digits(parts.next()?)?;
        for slot in read.iter_mut().skip(1) {
            match parts.next().and_then(digits) {
                Some(n) => *slot = n,
                None => break,
            }
        }
        Some(read)
    }
}

/// The digits a part begins with: `28` of `28`, `1` of `1-beta`, `6` of `6b`,
/// and nothing at all of `v0`.
fn digits(part: &str) -> Option<u32> {
    let taken = part.len() - part.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    part.get(..taken)?.parse().ok()
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
        terminals: names(answer),
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

/// Every name that came back, in the order it arrived. Bare, that is one or
/// none; under tmux it is tmux's own and then the outer terminal's.
fn names(bytes: &[u8]) -> Vec<Named> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some((named, after)) = next_name(rest) {
        out.extend(named);
        rest = after;
    }
    out
}

/// The next XTVERSION reply (`ESC P > | text ST`) and everything after it. An
/// unterminated one ends the walk: there is no knowing where it stops, and
/// what would follow it has not arrived either.
fn next_name(bytes: &[u8]) -> Option<(Option<Named>, &[u8])> {
    let start = find(bytes, b"\x1bP>|")? + 4;
    let end = start + find(&bytes[start..], b"\x1b\\")?;
    let text = std::str::from_utf8(&bytes[start..end]).ok();
    Some((text.and_then(named), &bytes[end + 2..]))
}

/// What a terminal called itself. A reply with no version at all is no
/// answer: a name alone cannot be held to a floor, and every list here is a
/// list of floors.
fn named(text: &str) -> Option<Named> {
    let (name, version) = split(text.trim())?;
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

    /// Under tmux 3.6b with `allow-passthrough on`, an outer Ghostty: tmux's
    /// own name first, then the four answers out of the envelope.
    const GHOSTTY_UNDER_TMUX: &[u8] = b"\x1bP>|tmux 3.6b\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[6;34;17t\x1bP>|ghostty 1.3.1\x1b\\\x1b[?62;22c";

    /// The same tmux with the passthrough off: the envelope is dropped whole
    /// and nothing the outer terminal would have said comes back.
    const TMUX_ALONE: &[u8] = b"\x1bP>|tmux 3.6b\x1b\\";

    fn one(name: &str, version: &str) -> Vec<Named> {
        vec![Named {
            name: name.into(),
            version: version.into(),
        }]
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
                terminals: one("kitty", "0.46.2"),
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
                terminals: one("ghostty", "1.3.1"),
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
                terminals: one("WezTerm", "20240203-110809-5046fc22"),
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
        assert_eq!(foot.terminals, one("foot", "1.28.0"));
        assert_eq!(parse(KONSOLE).terminals, one("Konsole", "26.08.0"));
    }

    /// The two shapes of the reply, and the ones that say nothing usable: a
    /// name with no version cannot be held to a floor, so it is no answer.
    #[test]
    fn the_name_is_read_from_either_shape_and_from_nothing_else() {
        let read = |reply: &str| {
            let bytes = format!("\x1bP>|{reply}\x1b\\\x1b[?62;c");
            parse(bytes.as_bytes()).terminals
        };
        assert_eq!(read("iTerm2 3.6.11"), one("iTerm2", "3.6.11"));
        assert_eq!(read("Rio 0.5.27"), one("Rio", "0.5.27"));
        assert_eq!(read(" kitty(0.28.0) "), one("kitty", "0.28.0"));
        assert!(read("kitty").is_empty(), "a name with no version");
        assert!(read("kitty()").is_empty(), "and an empty one");
        assert!(read("").is_empty());
        assert!(
            parse(b"\x1bP>|kitty(0.46.2)\x1b[?62;c")
                .terminals
                .is_empty(),
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
            query(Transport::Bare),
            [
                b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\".as_slice(),
                b"\x1b[16t".as_slice(),
                b"\x1b[>0q".as_slice(),
                b"\x1b[c".as_slice(),
            ]
            .concat()
        );
    }

    /// Under tmux the same four go inside one envelope, with every `ESC`
    /// doubled, behind one bare XTVERSION that tmux answers for itself.
    #[test]
    fn under_tmux_the_four_go_wrapped_behind_one_question_for_tmux() {
        assert_eq!(
            query(Transport::Tmux),
            [
                b"\x1b[>0q".as_slice(),
                b"\x1bPtmux;".as_slice(),
                b"\x1b\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\x1b\\".as_slice(),
                b"\x1b\x1b[16t\x1b\x1b[>0q\x1b\x1b[c".as_slice(),
                b"\x1b\\".as_slice(),
            ]
            .concat()
        );
    }

    /// Both names come back under tmux, in the order they arrive: tmux's own
    /// first, because it answers without asking anybody, then the outer
    /// terminal's out of the passthrough.
    #[test]
    fn every_name_that_answers_is_kept_in_the_order_it_arrived() {
        let probe = parse(GHOSTTY_UNDER_TMUX);
        assert_eq!(
            probe.terminals,
            vec![
                Named {
                    name: "tmux".into(),
                    version: "3.6b".into()
                },
                Named {
                    name: "ghostty".into(),
                    version: "1.3.1".into()
                },
            ]
        );
        assert!(probe.kitty);
        assert_eq!(
            probe.cell,
            Some(Cell {
                width: 17,
                height: 34
            })
        );
    }

    /// Passthrough off: the envelope is dropped whole, so tmux's own name is
    /// the only thing that comes back and there is no DA1 reply at all — the
    /// read ends on its clock, and what it has is one name.
    #[test]
    fn passthrough_off_leaves_nothing_but_tmuxs_own_name() {
        let probe = parse(TMUX_ALONE);
        assert_eq!(probe.terminals, one("tmux", "3.6b"));
        assert!(!probe.kitty);
        assert_eq!(probe.cell, None);
        assert!(!answered(TMUX_ALONE), "and nothing ended the read");
    }

    /// Two numbers, and a suffix on the third: read as far as it reads.
    #[test]
    fn a_version_is_read_as_far_as_it_is_numbers() {
        let read = |version: &str| {
            Named {
                name: "any".into(),
                version: version.into(),
            }
            .number()
        };
        assert_eq!(read("1.2"), Some([1, 2, 0]));
        assert_eq!(read("0.28.0-beta.3"), Some([0, 28, 0]));
        assert_eq!(read("2"), Some([2, 0, 0]));
        assert_eq!(read("3.6b"), Some([3, 6, 0]), "a running tmux");
        assert_eq!(read("1.x.3"), Some([1, 0, 0]));
        assert_eq!(read("20240203-110809-5046fc22"), Some([20240203, 0, 0]));
        assert_eq!(read("v0.2026.06"), None);
    }

    /// M60 brick 2: a reply that came in after the read ended joins the
    /// answer that was heard in time, and nothing already said is unsaid.
    #[test]
    fn a_late_reply_joins_the_answer_and_takes_nothing_back() {
        let heard = parse(TMUX_ALONE);
        let joined = heard
            .clone()
            .and(parse(b"\x1b_Gi=31;OK\x1b\\\x1bP>|ghostty 1.3.1\x1b\\"));
        assert!(joined.kitty, "the late `OK` is an `OK`");
        assert_eq!(
            joined.terminals,
            vec![
                Named {
                    name: "tmux".into(),
                    version: "3.6b".into()
                },
                Named {
                    name: "ghostty".into(),
                    version: "1.3.1".into()
                },
            ]
        );
        assert_eq!(
            joined.cell, None,
            "and the cell reply leaves no event to hear, so it is never late"
        );
        assert_eq!(
            joined.clone().and(parse(TMUX_ALONE)),
            joined,
            "the same name twice is the same terminal once"
        );
        assert_eq!(
            parse(KITTY).and(Probe::default()),
            parse(KITTY),
            "and nothing joined to an answer changes nothing"
        );
    }

    /// Brick 4: tmux answers `CSI 16 t` itself (`input.c` `case 16:`), so if
    /// one ever arrived beside the outer terminal's the first is the one
    /// taken. The probe never sends an unwrapped one, so the first is the
    /// outer terminal's — but the rule is pinned either way.
    #[test]
    fn the_first_cell_reply_is_the_one_taken() {
        assert_eq!(
            cell(b"\x1b[6;34;17t\x1b[6;20;10t"),
            Some(Cell {
                width: 17,
                height: 34
            })
        );
    }
}
