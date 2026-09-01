//! The ear a seat wears (ADR-0029 §1). One number says it: 0 is a live ear —
//! every post wakes the seat, today's seat and the default — and thirty
//! seconds or more is a patient one, whose posts land held and are read whole
//! at the seat's next turn. The band between is refused in words rather than
//! rounded: it names a live seat.
//!
//! A room's ears are two layers, and only two: what the roster declared when it
//! was seated, and what a seat has retuned for itself since (`Listen`). The
//! layers are folded here and nowhere else, so no caller can hold a second idea
//! of what a seat hears.

use std::collections::BTreeMap;
use std::time::Duration;

use bingo_sdk::{ErrorCode, KernelError, SessionState};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::chase::PATIENCE;
use crate::room::{EAR, LISTENERS, MEMBERS};
use crate::{PLUGIN, name};

/// The shortest patience a seat may ask for. Under it the storm bound a
/// patient ear buys is not worth the wait, so the doors refuse the number
/// instead of quietly rounding it (ADR-0029 §1).
pub const FLOOR: Duration = Duration::from_secs(30);

/// The key a patience is asked for and stored under, in every door.
pub const PATIENCE_S: &str = "patience_s";

/// How a seat hears its room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ear {
    /// Every post wakes it, as it lands.
    #[default]
    Live,
    /// Posts land held; the seat reads them whole at its next turn, and the
    /// deadline wakes it once when they have waited this long.
    Patient(Duration),
}

impl Ear {
    /// The ear a door asks for. A number under the floor is a live seat
    /// described the long way round, and is refused as one.
    pub fn asked(seconds: u64) -> Result<Ear, KernelError> {
        match seconds {
            0 => Ok(Ear::Live),
            _ if seconds < FLOOR.as_secs() => Err(dead_band(seconds)),
            _ => Ok(Ear::Patient(Duration::from_secs(seconds))),
        }
    }

    /// The ear a listener on a roster asks for: a name alone takes the
    /// default, which is the chaser's own patience and not a second constant.
    pub fn declared(seconds: Option<u64>) -> Result<Ear, KernelError> {
        match seconds {
            None => Ok(Ear::Patient(PATIENCE)),
            Some(seconds) => Ear::asked(seconds),
        }
    }

    /// The ear a journal has, which is never refused: what was written was
    /// already accepted at a door, and a number below the floor read back from
    /// an edited journal still asked to be patient, so it is held at the floor
    /// rather than dropped.
    pub fn stored(seconds: u64) -> Ear {
        match seconds {
            0 => Ear::Live,
            _ => Ear::Patient(Duration::from_secs(seconds.max(FLOOR.as_secs()))),
        }
    }

    pub fn is_live(self) -> bool {
        self == Ear::Live
    }

    /// The seconds a payload stores: a live ear is zero patience.
    pub fn seconds(self) -> u64 {
        match self {
            Ear::Live => 0,
            Ear::Patient(patience) => patience.as_secs(),
        }
    }

    /// What the seat is told it now wears.
    pub fn said(self) -> String {
        match self {
            Ear::Live => "every post wakes you as it lands".to_string(),
            Ear::Patient(patience) => format!(
                "posts wait for your next turn, and wake you once they have waited {}s",
                patience.as_secs()
            ),
        }
    }
}

/// A number nobody should have asked for, and what to ask for instead.
fn dead_band(seconds: u64) -> KernelError {
    KernelError::new(
        ErrorCode::InvalidInput,
        format!(
            "{seconds}s is under thirty seconds of patience: take the live seat you are \
             describing — 0 for an ear every post wakes, or {} and up for a patient one",
            FLOOR.as_secs()
        ),
    )
}

/// A seat on a roster: a name, and the ear it wears.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seat {
    pub name: String,
    pub ear: Ear,
}

impl Seat {
    /// One word of a roster line: `scout` is a live seat, `~parent` a patient
    /// one at the default, `~parent:120` at the patience it asks for.
    pub fn read(word: &str) -> Result<Seat, KernelError> {
        let Some(listening) = word.strip_prefix('~') else {
            return Ok(Seat::live(word));
        };
        let (name, asked) = match listening.split_once(':') {
            Some((name, seconds)) => (name, Some(patience(seconds)?)),
            None => (listening, None),
        };
        Ok(Seat {
            name: name.to_string(),
            ear: Ear::declared(asked)?,
        })
    }

    pub fn live(name: &str) -> Seat {
        Seat {
            name: name.to_string(),
            ear: Ear::Live,
        }
    }

    /// How the seat reads back, in a receipt and in `/room`: a patient ear
    /// wears the sigil that asked for it.
    pub fn said(&self) -> String {
        match self.ear {
            Ear::Live => self.name.clone(),
            Ear::Patient(patience) => format!("~{}({}s)", self.name, patience.as_secs()),
        }
    }
}

/// The seconds after a `~name:` — a word, not a number, is nobody's patience.
fn patience(seconds: &str) -> Result<u64, KernelError> {
    seconds.trim().parse().map_err(|_| {
        KernelError::new(
            ErrorCode::InvalidInput,
            format!("{seconds:?} is not a patience: `~name` takes the default, `~name:120` waits that many seconds"),
        )
    })
}

/// A listener as the structured doors declare one: a name, or a name and the
/// patience it asks for. `OpenRoom` and `.bingo/team.json` say it the same way.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum Listener {
    /// A name alone: a patient ear at the default.
    Name(String),
    Asked {
        name: String,
        patience_s: Option<u64>,
    },
}

impl Listener {
    /// The seat it declares, or the refusal its patience earns.
    pub fn seat(&self) -> Result<Seat, KernelError> {
        let (name, asked) = match self {
            Listener::Name(name) => (name, None),
            Listener::Asked { name, patience_s } => (name, *patience_s),
        };
        Ok(Seat {
            name: name.clone(),
            ear: Ear::declared(asked)?,
        })
    }
}

/// The roster a door declares: its members, live unless its listeners say
/// otherwise. A name in both is one seat, and the ear is the listener's.
pub fn seats(members: &[String], listeners: &[Listener]) -> Result<Vec<Seat>, KernelError> {
    let mut seats: Vec<Seat> = members.iter().map(|m| Seat::live(m)).collect();
    for listener in listeners {
        let seat = listener.seat()?;
        match seats
            .iter_mut()
            .find(|held| name::same(&held.name, &seat.name))
        {
            Some(held) => held.ear = seat.ear,
            None => seats.push(seat),
        }
    }
    Ok(seats)
}

/// Every ear in a room. The declaration is replaced whole by the next roster;
/// a retuning is one seat's own, and stands until that seat or a reseat writes
/// over it — so the two layers are kept apart and the fold reads them in that
/// order, whatever order the frames arrived in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ears {
    declared: BTreeMap<String, Ear>,
    retuned: BTreeMap<String, Ear>,
}

impl Ears {
    /// What a seat hears. The one answer to that question in this crate.
    pub fn of(&self, member: &str) -> Ear {
        let key = key(member);
        self.retuned
            .get(&key)
            .or_else(|| self.declared.get(&key))
            .copied()
            .unwrap_or_default()
    }

    /// The whole of a roster's declaration, from the membership payload it was
    /// published as.
    pub fn declare(&mut self, payload: &Value) {
        self.declared = declared(payload);
    }

    /// One seat's own retuning, as its `ear:` payload has it. A cleared one —
    /// what a reseat writes — leaves the seat with what the roster declared.
    pub fn retune(&mut self, member: &str, payload: &Value) {
        match stored(payload) {
            Some(ear) => self.retuned.insert(key(member), ear),
            None => self.retuned.remove(&key(member)),
        };
    }

    /// The seats a reseat has to clear: a roster declared whole replaces the
    /// ears with it, and a retuning is only cleared where it was written.
    pub fn retuned(&self) -> Vec<String> {
        self.retuned.keys().cloned().collect()
    }

    /// Whether anyone here listens. A room of live seats holds no deadline, so
    /// a reader can stop at this rather than walk the tree to ask who a
    /// session is on the roster — which is every room opened before there were
    /// ears, and every one that never asked for one.
    pub fn patient(&self) -> bool {
        self.declared
            .values()
            .chain(self.retuned.values())
            .any(|ear| !ear.is_live())
    }
}

/// A name as the ears are keyed: a room compares names in any case, so the
/// key is one spelling of it.
fn key(member: &str) -> String {
    member.to_lowercase()
}

/// The kind one seat's ear is published under. A kind per seat, so two seats
/// retuning at once write two facts rather than racing over one (ADR-0029 §4).
pub fn kind(member: &str) -> String {
    format!("{EAR}{}", key(member))
}

/// What a retuning is published as, and what clears one.
pub fn register(ear: Ear) -> Value {
    json!({ PATIENCE_S: ear.seconds() })
}

/// The listeners a membership payload names. A payload without them — every
/// room opened before there were ears — declares an all-live roster.
fn declared(payload: &Value) -> BTreeMap<String, Ear> {
    payload[LISTENERS]
        .as_array()
        .map(|listeners| listeners.iter().filter_map(listed).collect())
        .unwrap_or_default()
}

/// One entry of a payload's `listeners`: a name and the patience beside it.
fn listed(entry: &Value) -> Option<(String, Ear)> {
    let name = entry["name"].as_str()?;
    let ear = match entry[PATIENCE_S].as_u64() {
        Some(seconds) => Ear::stored(seconds),
        None => Ear::Patient(PATIENCE),
    };
    Some((key(name), ear))
}

/// The ear a register holds, or nothing for one that was cleared.
fn stored(payload: &Value) -> Option<Ear> {
    payload[PATIENCE_S].as_u64().map(Ear::stored)
}

/// The listeners a roster is published with: the seats that are not live, and
/// nothing at all when none of them are — a room of live seats is written
/// exactly as it was before there were ears.
pub fn listeners_of(seats: &[Seat]) -> Option<Value> {
    let listed: Vec<Value> = seats
        .iter()
        .filter(|seat| !seat.ear.is_live())
        .map(|seat| json!({ "name": seat.name, PATIENCE_S: seat.ear.seconds() }))
        .collect();
    (!listed.is_empty()).then(|| Value::Array(listed))
}

/// Every ear a room's own journal has: what its roster declared, and every
/// retuning published since. Both come from the one snapshot, so neither can
/// drift from the other.
pub fn ears_of(state: &SessionState) -> Ears {
    let mut ears = Ears::default();
    let Some(kinds) = state.extensions.get(PLUGIN) else {
        return ears;
    };
    if let Some(payload) = kinds.get(MEMBERS) {
        ears.declare(payload);
    }
    for (kind, payload) in kinds {
        if let Some(member) = kind.strip_prefix(EAR) {
            ears.retune(member, payload);
        }
    }
    ears
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::PARENT;
    use crate::room;

    fn seat(name: &str, seconds: u64) -> Seat {
        Seat {
            name: name.into(),
            ear: Ear::Patient(Duration::from_secs(seconds)),
        }
    }

    #[test]
    fn a_number_is_the_whole_of_the_dial_and_the_dead_band_is_refused() {
        assert_eq!(Ear::asked(0), Ok(Ear::Live));
        assert_eq!(Ear::asked(30), Ok(Ear::Patient(FLOOR)));
        assert_eq!(Ear::asked(300), Ok(Ear::Patient(PATIENCE)));
        assert_eq!(
            Ear::declared(None),
            Ok(Ear::Patient(PATIENCE)),
            "the default is the chaser's own patience"
        );

        let refused = Ear::asked(15).expect_err("under the floor");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(
            refused.message.contains("under thirty seconds of patience"),
            "{refused}"
        );
        assert!(refused.message.contains("live seat"), "{refused}");
        assert!(Ear::asked(29).is_err());
    }

    #[test]
    fn a_journal_is_read_rather_than_refused() {
        assert_eq!(Ear::stored(0), Ear::Live);
        assert_eq!(Ear::stored(15), Ear::Patient(FLOOR), "held at the floor");
        assert_eq!(Ear::stored(120), Ear::Patient(Duration::from_secs(120)));
    }

    #[test]
    fn a_roster_word_says_the_ear_beside_the_name() {
        assert_eq!(Seat::read("scout"), Ok(Seat::live("scout")));
        assert_eq!(Seat::read("~parent"), Ok(seat(PARENT, 300)));
        assert_eq!(Seat::read("~parent:120"), Ok(seat(PARENT, 120)));
        assert_eq!(Seat::read("~parent:0"), Ok(Seat::live(PARENT)));

        let clamped = Seat::read("~parent:15").expect_err("the dead band, not a clamp");
        assert!(
            clamped.message.contains("under thirty seconds of patience"),
            "{clamped}"
        );
        assert!(
            Seat::read("~parent:soon").is_err(),
            "a patience is a number"
        );
    }

    #[test]
    fn a_seat_reads_back_wearing_the_sigil_that_asked_for_it() {
        assert_eq!(Seat::live("scout").said(), "scout");
        assert_eq!(seat(PARENT, 300).said(), "~parent(300s)");
    }

    /// The two structured doors declare a listener the same way.
    #[test]
    fn a_listener_is_a_name_or_a_name_and_a_patience() {
        let named: Listener = serde_json::from_value(json!("parent")).expect("a name");
        assert_eq!(named.seat(), Ok(seat(PARENT, 300)));

        let asked: Listener =
            serde_json::from_value(json!({"name": "parent", "patience_s": 120})).expect("a seat");
        assert_eq!(asked.seat(), Ok(seat(PARENT, 120)));

        let bare: Listener =
            serde_json::from_value(json!({"name": "parent"})).expect("a seat with no number");
        assert_eq!(bare.seat(), Ok(seat(PARENT, 300)));

        let refused: Listener =
            serde_json::from_value(json!({"name": "parent", "patience_s": 15})).expect("a seat");
        assert!(refused.seat().is_err(), "the dead band, at every door");
    }

    #[test]
    fn a_door_s_roster_is_its_members_with_the_listeners_ears() {
        let members = ["scout", "parent"].map(str::to_string).to_vec();
        let listeners = [Listener::Name(PARENT.into())];
        assert_eq!(
            seats(&members, &listeners).expect("a roster"),
            [Seat::live("scout"), seat(PARENT, 300)],
            "a name in both is one seat, wearing the listener's ear"
        );
        assert_eq!(
            seats(&["scout".into()], &listeners).expect("a roster"),
            [Seat::live("scout"), seat(PARENT, 300)],
            "a listener nobody listed is seated by listening"
        );
    }

    /// The whole of the fold, as one table: what the roster declared, what a
    /// seat retuned for itself, and what a reseat leaves standing.
    #[test]
    fn an_ear_is_the_roster_s_until_the_seat_says_otherwise() {
        let mut ears = Ears::default();
        ears.declare(&room::payload(&[Seat::live("scout"), seat(PARENT, 120)]));
        assert_eq!(ears.of("scout"), Ear::Live);
        assert_eq!(ears.of(PARENT), Ear::Patient(Duration::from_secs(120)));
        assert_eq!(ears.of("PARENT"), Ear::Patient(Duration::from_secs(120)));
        assert_eq!(ears.of("nobody"), Ear::Live, "a name off the roster");

        ears.retune("scout", &register(Ear::Patient(FLOOR)));
        assert_eq!(ears.of("scout"), Ear::Patient(FLOOR));
        assert_eq!(ears.retuned(), ["scout"]);

        ears.retune("scout", &Value::Null);
        assert_eq!(ears.of("scout"), Ear::Live, "a cleared retuning");
        assert!(ears.retuned().is_empty());

        ears.declare(&room::payload(&[Seat::live(PARENT)]));
        assert_eq!(ears.of(PARENT), Ear::Live, "a roster is declared whole");
    }

    /// A room of live seats is a room the deadline never has to place: the
    /// reader says so without asking the tree anything.
    #[test]
    fn a_room_says_whether_anyone_in_it_listens_at_all() {
        let mut ears = Ears::default();
        ears.declare(&room::payload(&[Seat::live("scout")]));
        assert!(!ears.patient(), "every seat hears every post");

        ears.retune("scout", &register(Ear::Patient(FLOOR)));
        assert!(ears.patient());

        ears.retune("scout", &Value::Null);
        assert!(!ears.patient());

        ears.declare(&room::payload(&[seat(PARENT, 120)]));
        assert!(ears.patient(), "the roster declared one");
    }

    /// A retuning is written where the roster cannot reach it, so the order the
    /// frames arrive in — a reopened session restates them by kind, not by
    /// journal order — decides nothing.
    #[test]
    fn the_two_layers_are_read_in_their_own_order_whatever_order_they_arrived_in() {
        let roster = room::payload(&[Seat::live(PARENT)]);
        let retuning = register(Ear::Patient(FLOOR));

        let mut first = Ears::default();
        first.declare(&roster);
        first.retune(PARENT, &retuning);

        let mut second = Ears::default();
        second.retune(PARENT, &retuning);
        second.declare(&roster);

        assert_eq!(first, second);
        assert_eq!(first.of(PARENT), Ear::Patient(FLOOR));
    }

    #[test]
    fn a_seat_s_ear_is_published_under_a_kind_of_its_own() {
        assert_eq!(kind("Parent"), "ear:parent");
        assert_eq!(register(Ear::Live), json!({ "patience_s": 0 }));
        assert_eq!(register(Ear::Patient(FLOOR)), json!({ "patience_s": 30 }));
    }
}
