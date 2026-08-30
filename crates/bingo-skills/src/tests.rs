//! What every test in this crate needs: a tree of skill directories, and the
//! three contexts the sdk hands a command, a tool and a contributor.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, Attachment, CancellationToken, Catalog, CatalogKind, ClientIdentity,
    CloseReason, CommandContext, ContextQuery, ContextUsage, Delivery, Env, GatewayStream, HostApi,
    HostHandle, Input, IntentId, InteractionKind, Item, ItemBody, ItemId, KernelError,
    ModelCapabilities, OpenOptions, Prompter, SessionFilter, SessionId, SessionSelector,
    SessionSummary, ToolContext, ToolHost, TurnId, Usage,
};
use jiff::Timestamp;

use crate::scan::SKILL_FILE;

/// A machine with skills on it: a home holding the person's own layer, and a
/// working directory to run in.
pub(crate) struct Tree(tempfile::TempDir);

impl Tree {
    pub(crate) fn new() -> Self {
        Self(tempfile::tempdir().expect("a temporary home"))
    }

    /// The home the `Env` is rooted at.
    pub(crate) fn root(&self) -> PathBuf {
        self.0.path().to_path_buf()
    }

    /// Where a session in this tree works.
    pub(crate) fn cwd(&self) -> PathBuf {
        self.dir("work")
    }

    /// A directory under the home, created.
    pub(crate) fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root().join(relative);
        std::fs::create_dir_all(&path).expect("a directory");
        path
    }

    /// The person's own layer, `<home>/.bingo/skills`.
    pub(crate) fn user_layer(&self) -> PathBuf {
        self.root().join(".bingo").join("skills")
    }

    /// `<layer>/<name>/SKILL.md`, returning the skill's directory.
    pub(crate) fn skill(&self, layer: &Path, name: &str, source: &str) -> PathBuf {
        let dir = layer.join(name);
        std::fs::create_dir_all(&dir).expect("a skill directory");
        std::fs::write(dir.join(SKILL_FILE), source).expect("a SKILL.md");
        dir
    }

    pub(crate) fn user_skill(&self, name: &str, source: &str) -> PathBuf {
        self.skill(&self.user_layer(), name, source)
    }

    /// A skill in the project layer of `<home>/<at>`, returning that working
    /// directory.
    pub(crate) fn project_skill(&self, at: &str, name: &str, source: &str) -> PathBuf {
        let cwd = self.dir(at);
        self.skill(&cwd.join(".bingo").join("skills"), name, source);
        cwd
    }
}

/// A command context reads its session, its directory and a host; a skill
/// command asks the host nothing, so every answer here would be a bug.
struct UnusedHost;

#[async_trait]
impl HostApi for UnusedHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        unreachable!("a skill reads no session list")
    }

    async fn open(
        &self,
        _selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        unreachable!("a skill opens no session")
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        unreachable!("a skill closes no session")
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        unreachable!("a skill deletes no session")
    }

    async fn deliver(
        &self,
        _to: &SessionId,
        _intent: IntentId,
        _input: Input,
        _delivery: Delivery,
    ) -> Result<(), KernelError> {
        unreachable!("this double delivers nothing")
    }

    async fn extend(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double extends nothing")
    }

    async fn catalog(&self, _kind: CatalogKind) -> Result<Catalog, KernelError> {
        unreachable!("a skill reads no catalog")
    }

    fn gateway_events(&self) -> GatewayStream {
        unreachable!("a skill watches no gateway")
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

pub(crate) fn command_context() -> CommandContext {
    CommandContext {
        session: SessionId::from_raw("ses_test"),
        cwd: PathBuf::from("/work/project"),
        host: HostHandle(Arc::new(UnusedHost)),
    }
}

/// A tool host that is never asked anything: `Skill` reads a file and returns.
struct NullHost;

#[async_trait]
impl Prompter for NullHost {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        unreachable!("a skill asks nobody anything")
    }
}

#[async_trait]
impl ToolHost for NullHost {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        unreachable!("a skill records nothing of its own")
    }
}

pub(crate) fn tool_context(cwd: &Path) -> ToolContext {
    ToolContext {
        call_id: "call_test".into(),
        session: SessionId::from_raw("ses_test"),
        turn: TurnId::from_raw("trn_test"),
        item: ItemId::from_raw("itm_test"),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        env: Arc::new(Env::rooted("/nowhere")),
        host: bingo_sdk::testing::NoHost::handle(),
        call: Arc::new(NullHost),
    }
}

/// What a contributor is asked, when the test only cares where the session is
/// working.
pub(crate) struct Asked {
    session: SessionSummary,
    turn: TurnId,
    items: Vec<Item>,
    usage: ContextUsage,
    capabilities: ModelCapabilities,
    cwd: PathBuf,
}

pub(crate) fn asked(cwd: &Path) -> Asked {
    Asked {
        session: SessionSummary {
            driver: Default::default(),
            id: SessionId::from_raw("ses_test"),
            key: None,
            title: None,
            cwd: cwd.display().to_string(),
            parent: None,
            model: None,
            provider: None,
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
            usage: Usage::default(),
            busy: false,
        },
        turn: TurnId::from_raw("trn_test"),
        items: Vec::new(),
        usage: ContextUsage {
            used: 0,
            window: 100_000,
            trigger: 90_000,
        },
        capabilities: ModelCapabilities {
            context_window: 100_000,
            max_output: 8_000,
            images: false,
            reasoning: false,
            count_tokens: false,
            caching: false,
        },
        cwd: cwd.to_path_buf(),
    }
}

impl Asked {
    pub(crate) fn query(&self) -> ContextQuery<'_> {
        ContextQuery {
            session: &self.session,
            turn: &self.turn,
            round: 0,
            items: &self.items,
            usage: &self.usage,
            capabilities: &self.capabilities,
            cwd: &self.cwd,
        }
    }
}
