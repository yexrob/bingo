//! What the tests in this crate share: a home on disk with a library over it,
//! and the context the sdk hands a tool. Nothing here reaches a host — an
//! experience is files and this process, and a tool that asked the kernel for
//! anything would fail loudly here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, CancellationToken, Env, InteractionKind, ItemBody, ItemId, KernelError,
    Prompter, SessionId, ToolContext, ToolHost, ToolOutput, TurnId, testing::NoHost,
};

use crate::store::{Library, Shelf};

pub(crate) struct Fixture {
    home: tempfile::TempDir,
    pub(crate) library: Arc<Library>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().expect("a temp home");
        let env = Env::rooted(home.path());
        let library = Arc::new(Library::new(&env.config_dir));
        Self { home, library }
    }

    pub(crate) fn cwd(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    pub(crate) fn shelf(&self) -> Shelf {
        self.library.load(&self.cwd())
    }

    pub(crate) fn dir(&self) -> PathBuf {
        self.library.dir(&self.cwd())
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
        unreachable!("an experience tool asks nobody anything")
    }
}

#[async_trait]
impl ToolHost for Silent {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        unreachable!("an experience tool records nothing of its own")
    }
}

/// The text a tool answered with, as the model reads it.
pub(crate) fn text(out: &ToolOutput) -> String {
    out.parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect()
}

/// The files in a project's store, in name order.
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
