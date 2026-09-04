//! `/rewind`: the turns of this session, and going back to one.
//!
//! Bare, it is a table — which turns there are, what was asked in each and
//! which files it changed. With a turn, it puts those files back and then
//! asks the kernel to take the conversation back with them (ADR-0045 §3).
//! The files go first: a restore that fails stops before the journal is
//! touched, so nothing is ever undone in the transcript alone.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, ClientIdentity, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode,
    KernelError, OpenOptions, SessionId, SessionSelector, SessionState, TurnId, View,
};

use crate::restore::{self, Plan};
use crate::store::{Checkpoints, MOST};
use crate::turns::{self, Turn};

/// The name the TUI's `esc esc` picker looks for in the catalogue, and the
/// line it submits.
pub const NAME: &str = "rewind";

#[derive(Debug)]
pub struct RewindCommand {
    store: Arc<Checkpoints>,
}

impl RewindCommand {
    pub fn new(store: Arc<Checkpoints>) -> Self {
        Self { store }
    }

    /// The turns, and what each one changed on disk.
    fn listing(&self, session: &SessionId, state: &SessionState, cwd: &Path) -> View {
        let rows = turns::of(state)
            .iter()
            .rev()
            .map(|turn| {
                let touched = self.store.entries(session, &turn.id);
                vec![
                    turn.id.as_str().to_string(),
                    turn.label(),
                    named(touched.iter().map(|entry| entry.path.as_path()), cwd),
                ]
            })
            .collect();
        View::Table {
            headers: vec!["turn".into(), "asked".into(), "files".into()],
            rows,
        }
    }

    /// Put the files back, then take the conversation back to `wanted`.
    async fn go_back(
        &self,
        wanted: &TurnId,
        state: &SessionState,
        cx: &CommandContext,
    ) -> Result<CommandOutcome, KernelError> {
        let listed = turns::of(state);
        let undone = turns::from(&listed, wanted).ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                format!("this session has no turn {wanted}"),
            )
        })?;
        let plan = restore::plan(&self.store, &cx.session, &undone)?;
        restore::apply(&plan)?;
        let dropped = cx.host.rewind(&cx.session, wanted).await?;
        let to = listed.iter().find(|turn| &turn.id == wanted);
        Ok(CommandOutcome::Applied {
            message: Some(report(to, dropped, &plan, &cx.cwd)),
        })
    }
}

#[async_trait]
impl Command for RewindCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: NAME.into(),
            aliases: Vec::new(),
            hint: "go back to a turn: its files and the conversation".into(),
            args: ArgSpec::Free {
                hint: "<turn>".into(),
            },
            // The kernel refuses a rewind under a running turn, so this waits
            // in the queue for one rather than being refused for asking.
            instant: false,
            family: "session".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let state = transcript(cx).await?;
        match args.trim() {
            "" => Ok(CommandOutcome::View {
                view: self.listing(&cx.session, &state, &cx.cwd),
            }),
            wanted => self.go_back(&TurnId::from_raw(wanted), &state, cx).await,
        }
    }
}

/// The session's own fold, as the kernel already has it.
async fn transcript(cx: &CommandContext) -> Result<SessionState, KernelError> {
    let attachment = cx
        .host
        .open(
            SessionSelector::ById {
                id: cx.session.clone(),
            },
            ClientIdentity {
                name: NAME.into(),
                surface: "checkpoints".into(),
            },
            OpenOptions::default(),
        )
        .await?;
    Ok(attachment.snapshot)
}

/// What was done, in the words of the person who asked for it.
fn report(to: Option<&Turn>, dropped: u32, plan: &Plan, cwd: &Path) -> String {
    let mut lines = vec![format!(
        "rewound to {}, {} dropped",
        to.map(Turn::label).unwrap_or_else(|| "the turn".into()),
        items(dropped)
    )];
    if plan.is_empty() {
        lines.push("no file was changed in those turns".into());
    }
    if !plan.put_back.is_empty() {
        let paths = plan.put_back.iter().map(|(path, _)| path.as_path());
        lines.push(format!("put back {}", named(paths, cwd)));
    }
    if !plan.remove.is_empty() {
        lines.push(format!(
            "removed {}",
            named(plan.remove.iter().map(PathBuf::as_path), cwd)
        ));
    }
    if !plan.skipped.is_empty() {
        lines.push(format!(
            "left as it is, never kept (over {} MiB): {}",
            MOST / 1024 / 1024,
            named(plan.skipped.iter().map(PathBuf::as_path), cwd)
        ));
    }
    lines.join("\n")
}

fn items(n: u32) -> String {
    match n {
        1 => "1 item".into(),
        many => format!("{many} items"),
    }
}

/// Paths as a person reads them: relative to the session's own directory
/// where they are under it, whole where they are not.
fn named<'a>(paths: impl Iterator<Item = &'a Path>, cwd: &Path) -> String {
    paths
        .map(|path| path.strip_prefix(cwd).unwrap_or(path).display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Fixture;

    async fn run(fixture: &Fixture, args: &str) -> Result<CommandOutcome, KernelError> {
        RewindCommand::new(fixture.store.clone())
            .run(args, &fixture.command())
            .await
    }

    fn applied(outcome: CommandOutcome) -> String {
        match outcome {
            CommandOutcome::Applied { message } => message.unwrap_or_default(),
            other => panic!("/rewind <turn> applies, it does not answer {other:?}"),
        }
    }

    /// A file that was there before the first turn, edited in both, and a
    /// second file the later turn created.
    fn two_edited_turns(fixture: &Fixture) -> (PathBuf, PathBuf) {
        std::fs::write(fixture.cwd.join("note.md"), b"original").expect("a file");
        let note = fixture.edit("trn_1", "note.md", b"first");
        fixture.edit("trn_2", "note.md", b"second");
        let made = fixture.edit("trn_2", "made.md", b"new");
        (note, made)
    }

    #[tokio::test]
    async fn a_bare_rewind_lists_the_turns_newest_first_with_what_each_touched() {
        let fixture = Fixture::new();
        two_edited_turns(&fixture);
        let CommandOutcome::View { view } = run(&fixture, "").await.expect("a view") else {
            panic!("a bare /rewind is a table");
        };
        assert_eq!(
            view.fold(),
            "turn · asked · files\n\
             trn_2 · and rename it · note.md, made.md\n\
             trn_1 · write the note · note.md"
        );
    }

    #[tokio::test]
    async fn going_back_puts_the_files_back_and_then_the_conversation() {
        let fixture = Fixture::new();
        let (note, made) = two_edited_turns(&fixture);

        let message = applied(run(&fixture, "trn_1").await.expect("a rewind"));
        assert_eq!(
            message,
            "rewound to write the note, 4 items dropped\n\
             put back note.md\n\
             removed made.md"
        );
        assert_eq!(
            std::fs::read(&note).expect("read back"),
            b"original",
            "the oldest snapshot wins across both turns"
        );
        assert!(!made.exists(), "a file the turns created is gone again");
        assert_eq!(fixture.journal.rewound(), [TurnId::from_raw("trn_1")]);
        assert!(
            fixture.journal.items().is_empty(),
            "and so is the transcript"
        );
    }

    #[tokio::test]
    async fn going_back_to_the_last_turn_leaves_the_ones_before_it_alone() {
        let fixture = Fixture::new();
        let (note, _) = two_edited_turns(&fixture);
        applied(run(&fixture, "trn_2").await.expect("a rewind"));
        assert_eq!(std::fs::read(&note).expect("read back"), b"first");
        assert_eq!(fixture.journal.items(), ["itm_1", "itm_2"]);
    }

    #[tokio::test]
    async fn a_turn_this_session_never_had_moves_nothing() {
        let fixture = Fixture::new();
        let (note, _) = two_edited_turns(&fixture);
        let refused = run(&fixture, "trn_9").await.expect_err("no such turn");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert_eq!(std::fs::read(&note).expect("read back"), b"second");
        assert!(fixture.journal.rewound().is_empty());
    }

    /// The files go first, so a restore that cannot be read stops before the
    /// journal is touched: a transcript that says a turn was undone while the
    /// files still hold it would be the one unrecoverable state.
    #[tokio::test]
    async fn a_restore_that_cannot_be_read_leaves_the_journal_alone() {
        let fixture = Fixture::new();
        two_edited_turns(&fixture);
        std::fs::remove_file(
            fixture
                .store
                .root()
                .join("ses_one")
                .join("trn_1")
                .join("1.snap"),
        )
        .expect("the snapshot goes missing");

        let refused = run(&fixture, "trn_1")
            .await
            .expect_err("nothing to put back");
        assert_eq!(refused.code, ErrorCode::Storage);
        assert!(fixture.journal.rewound().is_empty());
        assert_eq!(fixture.journal.items().len(), 4);
    }

    #[test]
    fn the_spec_is_the_one_the_pickers_chord_looks_for() {
        let store = Arc::new(Checkpoints::new(Path::new("/nowhere")));
        let spec = RewindCommand::new(store).spec();
        assert_eq!(spec.name, "rewind");
        assert_eq!(
            spec.args,
            ArgSpec::Free {
                hint: "<turn>".into()
            }
        );
        assert!(
            !spec.instant,
            "a rewind waits for the turn it would have cut the ground from under"
        );
    }

    fn plan() -> Plan {
        Plan {
            put_back: vec![(PathBuf::from("/work/src/lib.rs"), b"before".to_vec())],
            remove: vec![PathBuf::from("/work/notes.md")],
            skipped: vec![PathBuf::from("/elsewhere/big.bin")],
        }
    }

    fn turn() -> Turn {
        Turn {
            id: TurnId::from_raw("trn_1"),
            asked: Some("write the note".into()),
        }
    }

    #[test]
    fn the_reply_says_what_moved_and_what_did_not() {
        assert_eq!(
            report(Some(&turn()), 4, &plan(), Path::new("/work")),
            "rewound to write the note, 4 items dropped\n\
             put back src/lib.rs\n\
             removed notes.md\n\
             left as it is, never kept (over 8 MiB): /elsewhere/big.bin"
        );
    }

    #[test]
    fn a_turn_that_touched_no_file_says_so_rather_than_nothing() {
        assert_eq!(
            report(Some(&turn()), 1, &Plan::default(), Path::new("/work")),
            "rewound to write the note, 1 item dropped\nno file was changed in those turns"
        );
    }
}
