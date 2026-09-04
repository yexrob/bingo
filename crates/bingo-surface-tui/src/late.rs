//! An answer that came back after the probe that asked for it gave up.
//!
//! Both probes of [`crate::terminal::Tui::enter`] — the background colour and
//! the pictures — write a question to the terminal and read until the answer
//! or the clock. A terminal slower than the clock answers into crossterm's key
//! stream instead, and there the escape of every reply reads as an `alt` chord
//! and its body reads as typed characters (crossterm 0.29
//! `event/sys/unix/parse.rs`, measured 2026-09-04):
//!
//! | the reply | what crossterm makes of it |
//! | --- | --- |
//! | `ESC _ Gi=31;OK ESC \` | `alt+_`, `G i = 3 1 ; O K`, `alt+\` |
//! | `ESC P >|ghostty 1.3.1 ESC \` | `alt+P`, `> | g h o …`, `alt+\` |
//! | `ESC ] 11;rgb:… ESC \` | `alt+]`, `1 1 ; r g b …`, `alt+\` |
//! | `CSI 6;34;17t` | nothing: its parser drops it |
//! | `CSI ?62;22c` | nothing: it keeps DA1 to itself |
//! | `CSI ?997;1n` | nothing — and every key struck after it is held with it |
//!
//! So the three `CSI` replies need no eating and cannot be read back either —
//! the cell a picture is sized by is the one part of an answer a late one
//! cannot carry (M60 Verified). The other three are what this hears, before
//! any binding does: events in, and either the events the run is to handle or
//! the bytes of one whole reply.
//!
//! The last row is why a terminal is never asked to *report* its colour scheme
//! (mode 2031, M71): `parse_csi` answers `Ok(None)` for every `CSI ?` sequence
//! whose final byte is neither `u` nor `c`, and its parser reads that as an
//! unfinished sequence and keeps the buffer — so the report and everything
//! typed after it wait there for a byte that ends a sequence it can name, and
//! go when it comes. Measured in `crates/bingo/tests/pty.rs`.
//!
//! It gives up the moment a sequence stops looking like one of the three, so
//! a person who types `alt+_` and then a word keeps both.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What one event off the terminal turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    /// The run's, in the order they arrived: the event just read, and
    /// anything held back before it that turned out not to be a reply.
    Keys(Vec<Event>),
    /// Held back, because a reply may still be taking shape.
    More,
    /// A whole reply, spelled in the bytes the terminal sent.
    Answer(Vec<u8>),
}

/// The ear on the key stream: the events held back while a reply takes shape,
/// which is empty between replies and is the whole of the state.
#[derive(Debug, Default)]
pub struct Late {
    held: Vec<KeyEvent>,
}

/// The most one reply may be. An XTVERSION name and an OSC colour are both
/// well under forty characters; past this, whatever is arriving is not one of
/// the answers that were asked for and belongs to the person typing.
const MOST: usize = 128;

impl Late {
    /// One event, heard.
    pub fn hear(&mut self, event: Event) -> Heard {
        let Event::Key(key) = event else {
            return self.release(event);
        };
        match self.held.first().and_then(opened) {
            Some(shape) => self.reading(shape, key),
            None => self.opening(key),
        }
    }

    /// Nothing is being read yet: the one event that could start a reply is
    /// held, and every other is the person's.
    fn opening(&mut self, key: KeyEvent) -> Heard {
        if opened(&key).is_none() {
            return Heard::Keys(vec![Event::Key(key)]);
        }
        self.held.push(key);
        Heard::More
    }

    /// A reply is being read: this event ends it, belongs to it, or ends the
    /// pretence that there was one.
    fn reading(&mut self, shape: Shape, key: KeyEvent) -> Heard {
        if self.ends(shape, &key) {
            self.held.push(key);
            let answer = spelled(&self.held);
            self.held.clear();
            return Heard::Answer(answer);
        }
        if !self.body(shape, &key) {
            return self.release(Event::Key(key));
        }
        self.held.push(key);
        Heard::More
    }

    /// Whether this event is the reply's terminator: `ST` as one `alt+\`, `ST`
    /// whose `ESC` a read boundary split off, or the `BEL` an OSC may end
    /// with. A reply with no body between the two is no reply.
    fn ends(&self, shape: Shape, key: &KeyEvent) -> bool {
        if self.held.len() < 2 {
            return false;
        }
        if bel(key) {
            return shape == Shape::Osc;
        }
        match plain(key) {
            Some('\\') => self.held.last().is_some_and(is_esc),
            _ => alt(key) == Some('\\'),
        }
    }

    /// Whether this event is part of the reply's body: a character the shape
    /// allows where it fell, or the `ESC` of a terminator a read boundary cut
    /// in two.
    fn body(&self, shape: Shape, key: &KeyEvent) -> bool {
        if self.held.len() > MOST {
            return false;
        }
        if is_esc(key) {
            return !self.held.last().is_some_and(is_esc);
        }
        match plain(key) {
            Some(c) => shape.begins(self.held.len() - 1, c),
            None => false,
        }
    }

    /// It was not a reply: everything held, and then the event that said so,
    /// are the run's after all.
    fn release(&mut self, event: Event) -> Heard {
        let mut out: Vec<Event> = self.held.drain(..).map(Event::Key).collect();
        out.push(event);
        Heard::Keys(out)
    }
}

/// The three shapes a terminal's answer comes back in, named by the character
/// the `alt` chord that opens each carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `ESC _ … ST`: the kitty graphics query's answer.
    Apc,
    /// `ESC P … ST`: XTVERSION's answer.
    Dcs,
    /// `ESC ] … ST`: an OSC answer, which is the background colour's.
    Osc,
}

impl Shape {
    /// Whether the body may carry this character at this place. Only the
    /// first characters are pinned, and they are pinned to the answers this
    /// surface asked for: a kitty answer starts `G`, an XTVERSION reply `>|`,
    /// an OSC reply the number of the question it answers. Past those, a
    /// reply may say anything, and a person who typed the chord by hand has
    /// already been let go.
    fn begins(self, index: usize, c: char) -> bool {
        match (self, index) {
            (Shape::Apc, 0) => c == 'G',
            (Shape::Dcs, 0) => c == '>',
            (Shape::Dcs, 1) => c == '|',
            (Shape::Osc, 0) => c.is_ascii_digit(),
            _ => true,
        }
    }
}

/// Which shape this event opens, if any.
fn opened(key: &KeyEvent) -> Option<Shape> {
    match alt(key)? {
        '_' => Some(Shape::Apc),
        'P' => Some(Shape::Dcs),
        ']' => Some(Shape::Osc),
        _ => None,
    }
}

/// The bytes the terminal sent, read back out of what crossterm made of them:
/// `alt` and a character is an escape and that character, `esc` is an escape
/// on its own, and `ctrl+g` is the `BEL` an OSC may end with.
fn spelled(held: &[KeyEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    for key in held {
        match key.code {
            KeyCode::Esc => out.push(0x1b),
            KeyCode::Char(_) if bel(key) => out.push(0x07),
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::ALT) {
                    out.push(0x1b);
                }
                out.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes());
            }
            _ => {}
        }
    }
    out
}

/// The character of an `alt` chord. `shift` rides along on a capital, so it
/// is not part of the question.
fn alt(key: &KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let held = key.modifiers.difference(KeyModifiers::SHIFT);
    (pressed(key) && held == KeyModifiers::ALT).then_some(c)
}

/// The character of a plain keystroke: no modifier but the `shift` a capital
/// carries.
fn plain(key: &KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let held = key.modifiers.difference(KeyModifiers::SHIFT);
    (pressed(key) && held.is_empty()).then_some(c)
}

/// `ctrl+g`, which is the byte `BEL`.
fn bel(key: &KeyEvent) -> bool {
    pressed(key) && key.code == KeyCode::Char('g') && key.modifiers == KeyModifiers::CONTROL
}

fn is_esc(key: &KeyEvent) -> bool {
    pressed(key) && key.code == KeyCode::Esc
}

/// A reply's bytes are always a press; a release is somebody's finger.
fn pressed(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The events crossterm makes of `bytes`, which is how every case here is
    /// spelled: the reply as the terminal sends it, played through the same
    /// rules crossterm's parser follows — `ESC` and the byte after it are one
    /// `alt` chord, every other byte is its own key.
    fn events(bytes: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        let mut escaped = false;
        for byte in bytes {
            match (*byte, escaped) {
                (0x1b, false) => escaped = true,
                (0x07, _) => out.push(key('g', KeyModifiers::CONTROL)),
                (byte, alt) => {
                    let c = char::from(byte);
                    let mut held = match alt {
                        true => KeyModifiers::ALT,
                        false => KeyModifiers::empty(),
                    };
                    if c.is_uppercase() {
                        held |= KeyModifiers::SHIFT;
                    }
                    out.push(key(c, held));
                    escaped = false;
                }
            }
        }
        if escaped {
            out.push(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::empty(),
            )));
        }
        out
    }

    fn key(c: char, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), modifiers))
    }

    /// Everything one run of events came to: the answers heard whole, and the
    /// events that turned out to be the person's.
    fn heard(events: Vec<Event>) -> (Vec<String>, Vec<Event>) {
        let mut ear = Late::default();
        let (mut answers, mut keys) = (Vec::new(), Vec::new());
        for event in events {
            match ear.hear(event) {
                Heard::More => {}
                Heard::Keys(passed) => keys.extend(passed),
                Heard::Answer(bytes) => answers.push(String::from_utf8_lossy(&bytes).into_owned()),
            }
        }
        (answers, keys)
    }

    /// The graphics probe's own answer, byte for byte as M49's pty harness
    /// spells it: the kitty `OK`, the cell, the name, DA1. The two `CSI`
    /// replies make no events at all, so they are simply not there.
    const OUTER: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1bP>|ghostty 1.3.1\x1b\\";

    #[test]
    fn a_late_graphics_answer_is_heard_whole_and_nothing_of_it_is_typed() {
        let (answers, keys) = heard(events(OUTER));
        assert_eq!(
            answers,
            vec!["\x1b_Gi=31;OK\x1b\\", "\x1bP>|ghostty 1.3.1\x1b\\"]
        );
        assert!(keys.is_empty(), "{keys:?}");
    }

    /// tmux's own name arrives the same way, and so does the background
    /// colour the other probe asked for: one reply is one reply, whichever
    /// probe it belonged to.
    #[test]
    fn every_shape_a_probe_is_answered_in_is_heard() {
        for reply in [
            b"\x1bP>|tmux 3.6b\x1b\\".as_slice(),
            b"\x1bP>|kitty(0.46.2)\x1b\\".as_slice(),
            b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\".as_slice(),
            b"\x1b]10;rgb:cdd6/f4f4/f5f5\x1b\\".as_slice(),
        ] {
            let (answers, keys) = heard(events(reply));
            assert_eq!(answers, vec![String::from_utf8_lossy(reply)], "{reply:?}");
            assert!(keys.is_empty(), "{reply:?} left {keys:?}");
        }
    }

    /// An OSC an older terminal ends with `BEL` instead of `ST`: the byte
    /// reaches crossterm as `ctrl+g`, which is a binding of its own, so it
    /// must be eaten as the terminator it is.
    #[test]
    fn an_osc_that_ends_in_bel_is_heard_and_its_ctrl_g_never_fires() {
        let (answers, keys) = heard(events(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07"));
        assert_eq!(answers, vec!["\x1b]11;rgb:1e1e/1e1e/2e2e\x07"]);
        assert!(keys.is_empty(), "{keys:?}");
    }

    /// A read boundary between the `ESC` of the terminator and its `\` turns
    /// one `alt+\` into `esc` and `\` — and `esc` ends a turn. Both are part
    /// of the reply, so both are eaten.
    #[test]
    fn a_terminator_split_by_a_read_boundary_is_still_a_terminator() {
        let mut ear = Late::default();
        let mut before = Vec::new();
        for event in events(b"\x1b_Gi=31;OK") {
            before.push(ear.hear(event));
        }
        before.push(ear.hear(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::empty(),
        ))));
        let last = ear.hear(key('\\', KeyModifiers::empty()));
        assert_eq!(last, Heard::Answer(b"\x1b_Gi=31;OK\x1b\\".to_vec()));
        assert!(before.iter().all(|one| *one == Heard::More), "{before:?}");
    }

    /// A person's keystroke is a person's keystroke: the chords a reply opens
    /// with are held for exactly one event and then let go, in order, with
    /// nothing lost and nothing reordered.
    #[test]
    fn a_person_who_types_the_chord_by_hand_keeps_every_key() {
        let typed = vec![
            key('_', KeyModifiers::ALT),
            key('h', KeyModifiers::empty()),
            key('i', KeyModifiers::empty()),
        ];
        let (answers, keys) = heard(typed.clone());
        assert!(answers.is_empty());
        assert_eq!(keys, typed, "the chord and the word both arrive");
    }

    /// And a reply that stops looking like one hands back everything it was
    /// holding — the shape's own prefix is what says so on the second event.
    #[test]
    fn a_sequence_that_stops_looking_like_a_reply_is_given_back() {
        let typed = vec![
            key('P', KeyModifiers::ALT | KeyModifiers::SHIFT),
            key('>', KeyModifiers::empty()),
            key('x', KeyModifiers::empty()),
            key('\\', KeyModifiers::ALT),
        ];
        let (answers, keys) = heard(typed.clone());
        assert!(answers.is_empty(), "{answers:?}");
        assert_eq!(keys, typed);
    }

    /// Anything that is not a key ends a reply that was taking shape: a
    /// paste, a resize and a click all belong to the run, and so does
    /// whatever was held before them.
    #[test]
    fn an_event_that_is_no_key_at_all_gives_back_what_was_held() {
        let mut ear = Late::default();
        assert_eq!(ear.hear(key('_', KeyModifiers::ALT)), Heard::More);
        assert_eq!(
            ear.hear(Event::Resize(80, 24)),
            Heard::Keys(vec![key('_', KeyModifiers::ALT), Event::Resize(80, 24)])
        );
    }

    /// A terminal that opens a reply and never ends it holds nothing back for
    /// long: past [`MOST`] the ear gives up and the keys are the person's.
    #[test]
    fn a_reply_that_never_ends_is_bounded() {
        let mut events = vec![key('_', KeyModifiers::ALT), key('G', KeyModifiers::SHIFT)];
        events.extend((0..MOST + 8).map(|_| key('x', KeyModifiers::empty())));
        let (answers, keys) = heard(events);
        assert!(answers.is_empty());
        assert_eq!(keys.len(), MOST + 10, "everything held came back");
    }

    /// Two replies in a row: the ear is empty between them, so the second is
    /// heard as cleanly as the first.
    #[test]
    fn one_reply_leaves_nothing_behind_for_the_next() {
        let (answers, keys) = heard(events(b"\x1bP>|tmux 3.6b\x1b\\\x1b_Gi=31;OK\x1b\\"));
        assert_eq!(
            answers,
            vec!["\x1bP>|tmux 3.6b\x1b\\", "\x1b_Gi=31;OK\x1b\\"]
        );
        assert!(keys.is_empty());
    }

    /// An empty reply is no reply: `alt+_` then `alt+\` is two chords a
    /// person pressed, not an answer with nothing in it.
    #[test]
    fn a_reply_with_no_body_is_not_a_reply() {
        let typed = vec![key('_', KeyModifiers::ALT), key('\\', KeyModifiers::ALT)];
        let (answers, keys) = heard(typed.clone());
        assert!(answers.is_empty());
        assert_eq!(keys, typed);
    }

    /// A key release is a finger, never a reply's byte.
    #[test]
    fn a_release_is_never_part_of_a_reply() {
        let mut released = KeyEvent::new(KeyCode::Char('_'), KeyModifiers::ALT);
        released.kind = KeyEventKind::Release;
        let mut ear = Late::default();
        assert_eq!(
            ear.hear(Event::Key(released)),
            Heard::Keys(vec![Event::Key(released)])
        );
    }
}
