//! Inputs that arrived while the session was busy, drained one unit at a time
//! (ADR-0008 §2): a run of prose opens one turn, a command is one unit, and a
//! barrier absorbs prose only up to the first command.

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

    /// The prose at the head, up to the first command.
    pub(super) fn take_prose(&mut self) -> Vec<(IntentId, Input)> {
        let n = self
            .entries
            .iter()
            .take_while(|(_, input)| !commands::is_command(input))
            .count();
        self.entries.drain(..n).collect()
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
    let (text, origin) = match input {
        Input::Text { text, origin, .. } => (text.as_str(), origin.clone()),
        Input::Action { action } => (action.name.as_str(), Origin::default()),
    };
    QueueEntry {
        intent: intent.clone(),
        position,
        preview: text.chars().take(PREVIEW_CHARS).collect(),
        steerable: !commands::is_command(input),
        origin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> (IntentId, Input) {
        (IntentId::mint(), Input::text(t, Origin::surface("test")))
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
}
