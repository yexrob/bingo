//! What the tests in this crate share: a home on disk with a store over it,
//! and the context the sdk hands a tool. Nothing here reaches a host — a
//! tool over this store writes files and rings a bell, and one that asked
//! the kernel for anything would fail loudly here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, CancellationToken, CommandContext, Env, InteractionKind, ItemBody, ItemId,
    KernelError, Prompter, SessionId, ToolContext, ToolHost, ToolOutput, TurnId, testing::NoHost,
};

use crate::schedules::Schedules;
use crate::store::Shelf;

pub(crate) struct Fixture {
    home: tempfile::TempDir,
    pub(crate) schedules: Arc<Schedules>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().expect("a temp home");
        let env = Env::rooted(home.path());
        let schedules = Arc::new(Schedules::new(&env.data_dir));
        Self { home, schedules }
    }

    pub(crate) fn cwd(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    pub(crate) fn shelf(&self) -> Shelf {
        self.schedules.store().load()
    }

    pub(crate) fn dir(&self) -> PathBuf {
        self.schedules.store().dir().to_path_buf()
    }

    pub(crate) fn context(&self) -> ToolContext {
        ToolContext {
            call_id: "call_test".into(),
            session: SessionId::from_raw("ses_test"),
            turn: TurnId::from_raw("trn_test"),
            item: ItemId::from_raw("itm_test"),
            cwd: self.cwd(),
            cancel: CancellationToken::new(),
            env: Arc::new(Env::rooted(self.home.path())),
            host: NoHost::handle(),
            call: Arc::new(Silent),
        }
    }

    pub(crate) fn command(&self) -> CommandContext {
        CommandContext {
            session: SessionId::from_raw("ses_test"),
            cwd: self.cwd(),
            host: NoHost::handle(),
        }
    }
}

/// A tool here asks nobody anything and records nothing outside its result.
struct Silent;

#[async_trait]
impl Prompter for Silent {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        unreachable!("a schedule tool asks nobody anything")
    }
}

#[async_trait]
impl ToolHost for Silent {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        unreachable!("a schedule tool records nothing of its own")
    }
}

/// The text a tool answered with, as the model reads it.
pub(crate) fn text(out: &ToolOutput) -> String {
    out.parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect()
}

/// The files in the store, in name order.
pub(crate) fn files(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .flatten()
        .map(|file| file.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
