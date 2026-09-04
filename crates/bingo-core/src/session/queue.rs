//! Inputs that arrived while the session was busy, drained one unit at a time
//! (ADR-0008 §2): a run of prose opens one turn, a command is one unit, and a
//! barrier absorbs prose only up to the first line that will not steer — a
//! command, or a line that asked to wait for the turn to end (M68).

use std::collections::VecDeque;

use bingo_sdk::*;

use super::commands;

/// Characters of a queued input shown in `QueueChanged`.
const PREVIEW_CHARS: usize = 80;

#[derive(Default)]
pub(super) struct Queue {
    entries: VecDeque<(IntentId, Input)>,
    revision: u64,
}

/// What the head of the queue asks the idle session to do next.
pub(super) enum Unit {
    Prose(Vec<(IntentId, Input)>),
    Command(IntentId, Input),
}

impl Queue {
    /// Append; returns the 1-based position.
    pub(super) fn push(&mut self, intent: IntentId, input: Input) -> u32 {
        self.entries.push_back((intent, input));
        self.entries.len() as u32
    }

    /// Everything, for a session that is closing.
    pub(super) fn drain_all(&mut self) -> Vec<(IntentId, Input)> {
        self.entries.drain(..).collect()
    }

    /// The prose at the head, up to the first command: what a turn opening
    /// now takes with it. A held line is prose here — it asked to wait for a
    /// turn, and this is the turn it waited for.
    pub(super) fn take_prose(&mut self) -> Vec<(IntentId, Input)> {
        self.take_run(|input| !commands::is_command(input))
    }

    /// The prose a running turn may be steered with: up to the first command
    /// and the first line that asked to wait (M68).
    pub(super) fn take_steers(&mut self) -> Vec<(IntentId, Input)> {
        self.take_run(steerable)
    }

    /// The head of the queue for as long as `keep` holds.
    fn take_run(&mut self, keep: impl Fn(&Input) -> bool) -> Vec<(IntentId, Input)> {
        let n = self
            .entries
            .iter()
            .take_while(|(_, input)| keep(input))
            .count();
        self.entries.drain(..n).collect()
    }

    /// What `intent` is queued as, while it still is: what a withdraw reads
    /// before it decides whether the line is the caller's to take.
    pub(super) fn queued(&self, intent: &IntentId) -> Option<&Input> {
        self.entries
            .iter()
            .find(|(queued, _)| queued == intent)
            .map(|(_, input)| input)
    }

    /// Take `intent` out of the queue; `None` when it is not in it.
    pub(super) fn take(&mut self, intent: &IntentId) -> Option<Input> {
        let at = self.entries.iter().position(|(id, _)| id == intent)?;
        self.entries.remove(at).map(|(_, input)| input)
    }

    /// The next unit, when there is one.
    pub(super) fn take_unit(&mut self) -> Option<Unit> {
        let (_, head) = self.entries.front()?;
        if commands::is_command(head) {
            return self
                .entries
                .pop_front()
                .map(|(i, input)| Unit::Command(i, input));
        }
        Some(Unit::Prose(self.take_prose()))
    }

    /// The wire view after a change; every call is a new revision.
    pub(super) fn changed(&mut self) -> Event {
        self.revision += 1;
        Event::QueueChanged {
            revision: self.revision,
            entries: self
                .entries
                .iter()
                .enumerate()
                .map(|(i, (intent, input))| entry(intent, input, i as u32 + 1))
                .collect(),
        }
    }
}

fn entry(intent: &IntentId, input: &Input, position: u32) -> QueueEntry {
    QueueEntry {
        intent: intent.clone(),
        position,
        preview: preview(input),
        steerable: steerable(input),
        origin: origin_of(input),
    }
}

/// Whether a running turn may be steered with this line: not a command, and
/// not one that asked to wait for the turn to end (M68). It is what the wire's
/// `steerable` says and what the barrier obeys, from the one reading.
fn steerable(input: &Input) -> bool {
    !commands::is_command(input) && !input.is_held()
}

pub(super) fn origin_of(input: &Input) -> Origin {
    match input {
        Input::Text { origin, .. } => origin.clone(),
        Input::Action { .. } => Origin::default(),
    }
}

/// The words shown for a queued ask, cut to length; an ask that carries a
/// picture and no words previews as `(an image)`.
fn preview(input: &Input) -> String {
    let text = match input {
        Input::Text { text, images, .. } if text.is_empty() && !images.is_empty() => {
            return "(an image)".to_string();
        }
        Input::Text { text, .. } => text.as_str(),
        Input::Action { action } => action.name.as_str(),
    };
    text.chars().take(PREVIEW_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> (IntentId, Input) {
        (IntentId::mint(), Input::text(t, Origin::surface("test")))
    }

    /// A line that asked to wait for the turn to end (M68).
    fn held(t: &str) -> (IntentId, Input) {
        (
            IntentId::mint(),
            Input::Text {
                text: t.into(),
                images: Vec::new(),
                origin: Origin::surface("test"),
                delivery: Delivery::Hold,
            },
        )
    }

    fn push(q: &mut Queue, entry: (IntentId, Input)) -> IntentId {
        q.push(entry.0.clone(), entry.1);
        entry.0
    }

    #[test]
    fn prose_is_one_unit_and_a_command_is_another() {
        let mut q = Queue::default();
        q.push(text("a").0, text("a").1);
        q.push(text("b").0, text("b").1);
        q.push(text("/compact").0, text("/compact").1);
        q.push(text("c").0, text("c").1);
        assert!(matches!(q.take_unit(), Some(Unit::Prose(p)) if p.len() == 2));
        assert!(matches!(q.take_unit(), Some(Unit::Command(..))));
        assert!(matches!(q.take_unit(), Some(Unit::Prose(p)) if p.len() == 1));
        assert!(q.take_unit().is_none());
    }

    #[test]
    fn a_barrier_absorbs_prose_only_up_to_the_first_command() {
        let mut q = Queue::default();
        q.push(text("a").0, text("a").1);
        q.push(text("!ls").0, text("!ls").1);
        q.push(text("b").0, text("b").1);
        assert_eq!(q.take_prose().len(), 1);
        assert!(q.take_prose().is_empty(), "the command blocks the rest");
        assert!(matches!(q.take_unit(), Some(Unit::Command(..))));
    }

    #[test]
    fn the_wire_view_marks_commands_as_not_steerable() {
        let mut q = Queue::default();
        q.push(text("a").0, text("a").1);
        q.push(text("/x").0, text("/x").1);
        let Event::QueueChanged { revision, entries } = q.changed() else {
            panic!("a queue change");
        };
        assert_eq!(revision, 1);
        assert_eq!(entries[0].position, 1);
        assert!(entries[0].steerable);
        assert!(!entries[1].steerable);
    }

    #[test]
    fn an_image_only_ask_previews_as_an_image() {
        let image = Image::from_bytes("image/png", b"abc").expect("a small image");
        let input = Input::Text {
            text: String::new(),
            images: vec![image],
            origin: Origin::surface("test"),
            delivery: Delivery::Wake,
        };
        let mut q = Queue::default();
        q.push(IntentId::mint(), input);
        let Event::QueueChanged { entries, .. } = q.changed() else {
            panic!("a queue change");
        };
        assert_eq!(entries[0].preview, "(an image)");
    }

    /// The barrier is what `Hold` is about: a running turn is steered with
    /// the lines that steer and with nothing after the first that does not.
    #[test]
    fn a_barrier_stops_at_a_held_line_as_it_stops_at_a_command() {
        let mut q = Queue::default();
        push(&mut q, text("a"));
        push(&mut q, held("b"));
        push(&mut q, text("c"));
        assert_eq!(q.take_steers().len(), 1, "only the line that steers");
        assert!(q.take_steers().is_empty(), "the held line blocks the rest");
    }

    /// And the turn it waited for takes it: a held line is prose to whatever
    /// opens next, so two of them are one turn's inputs.
    #[test]
    fn the_turn_that_opens_next_takes_the_held_lines() {
        let mut q = Queue::default();
        push(&mut q, held("a"));
        push(&mut q, held("b"));
        push(&mut q, text("/compact"));
        assert!(matches!(q.take_unit(), Some(Unit::Prose(p)) if p.len() == 2));
        assert!(matches!(q.take_unit(), Some(Unit::Command(..))));
    }

    #[test]
    fn the_wire_view_marks_a_held_line_as_not_steerable() {
        let mut q = Queue::default();
        push(&mut q, text("a"));
        push(&mut q, held("b"));
        let Event::QueueChanged { entries, .. } = q.changed() else {
            panic!("a queue change");
        };
        assert!(entries[0].steerable);
        assert!(!entries[1].steerable, "it asked to wait");
    }

    /// What a withdraw reads and then takes; an intent that is not queued is
    /// nothing to either of them.
    #[test]
    fn a_queued_line_is_found_by_its_intent_and_taken_out_once() {
        let mut q = Queue::default();
        let first = push(&mut q, text("a"));
        let second = push(&mut q, text("b"));
        assert!(q.queued(&first).is_some());
        assert!(q.queued(&IntentId::mint()).is_none());
        let taken = q.take(&first).expect("the first line");
        assert_eq!(preview(&taken), "a");
        assert!(q.take(&first).is_none(), "once");
        let Event::QueueChanged { entries, .. } = q.changed() else {
            panic!("a queue change");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].intent, second);
        assert_eq!(entries[0].position, 1, "what is left is renumbered");
    }
}
