//! What every test here needs: the example binary, a home with it installed,
//! a manager started over that home, and the tool host one call is given.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bingo_plugin_rpc::Manager;
use bingo_sdk::{
    Answer, AnswerSpec, Attachment, CancellationToken, Catalog, CatalogKind, ClientIdentity,
    CloseReason, Delivery, Env, GatewayStream, HostApi, HostHandle, Input, IntentId,
    InteractionKind, ItemBody, ItemId, KernelError, Level, OpenOptions, Prompter, SessionFilter,
    SessionId, SessionSelector, SessionSummary, Tool, ToolContext, ToolError, ToolHost, ToolOutput,
    TurnId, WireService,
};
use serde_json::{Value, json};

/// The example binary, beside the test binary that is running.
pub fn stub_plugin() -> PathBuf {
    let test = std::env::current_exe().expect("a running test binary knows its own path");
    let profile = test
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives at target/<profile>/deps/<test>");
    let stub = profile.join("examples").join("stub_plugin");
    assert!(
        stub.exists(),
        "cargo test builds this crate's examples; {} is missing",
        stub.display()
    );
    stub
}

/// A home whose `<config_dir>/plugins/<name>` holds a manifest for the
/// example, once per plugin named, each run with its own arguments.
pub fn installed(plugins: &[(&str, &[&str])]) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a home");
    for (name, args) in plugins {
        let root = home.path().join(".bingo/plugins").join(name);
        std::fs::create_dir_all(&root).expect("a plugin directory");
        let manifest = json!({
            "name": name,
            "version": "0.1.0",
            "entry": {
                "command": stub_plugin().display().to_string(),
                "args": args,
                "env": { "PLUGIN_HOME": "${PLUGIN_ROOT}" }
            }
        });
        std::fs::write(root.join("plugin.json"), manifest.to_string()).expect("a manifest");
    }
    home
}

/// A started manager, the host its plugins reach, and the directories both
/// live in — held so they outlive the test.
pub struct Started {
    pub manager: Arc<Manager>,
    pub host: HostHandle,
    /// The same host, as the thing that heard whatever the bridge said.
    pub listener: Arc<Listening>,
    pub home: tempfile::TempDir,
    pub project: tempfile::TempDir,
}

impl Started {
    /// Wait for a notice with this code to be said, or give up. The one drain
    /// is a task of its own, so a test that asks about a notice polls, the way
    /// one that asks about a respawn does.
    pub async fn heard(&self, code: &str) -> (Level, String) {
        for _ in 0..300 {
            if let Some(said) = self.listener.heard(code) {
                return said;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("nothing said {code}: {:?}", self.listener.all());
    }

    /// Everything the bridge has said or is about to: what the drain has
    /// already put through the host, and whatever is still waiting for it.
    pub fn said(&self) -> Vec<(Level, String, String)> {
        let mut said = self.listener.all();
        said.extend(
            self.manager
                .notices()
                .drain()
                .into_iter()
                .map(|notice| (notice.level, notice.code, notice.text)),
        );
        said
    }
}

/// Those plugins, started over a host that keeps whatever services they open
/// and hears whatever the bridge says.
pub async fn started_with(plugins: &[(&str, &[&str])]) -> Started {
    let home = installed(plugins);
    let project = tempfile::tempdir().expect("a project");
    let manager = Arc::new(Manager::new(Env::rooted(home.path()), BTreeMap::new()));
    let listener = Arc::new(Listening::new());
    let host = HostHandle(Arc::clone(&listener) as Arc<dyn HostApi>);
    manager.start(project.path(), host.clone()).await;
    Started {
        manager,
        host,
        listener,
        home,
        project,
    }
}

/// The one stub, started, with a project beside it.
pub async fn started(args: &[&str]) -> (Arc<Manager>, tempfile::TempDir, tempfile::TempDir) {
    let started = started_with(&[("stub", args)]).await;
    (started.manager, started.home, started.project)
}

pub async fn only_tool(manager: &Manager) -> Arc<dyn Tool> {
    let mut tools = manager.tools().await;
    assert_eq!(tools.len(), 1, "the stub offers one tool");
    tools.remove(0)
}

/// Poll until the plugin offers a tool again, or give up. A respawn is
/// deliberately asynchronous, so a test that asks about it polls.
pub async fn respawned(manager: &Manager) -> Vec<Arc<dyn Tool>> {
    for _ in 0..300 {
        let tools = manager.tools().await;
        if !tools.is_empty() {
            return tools;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the plugin never came back");
}

// --------------------------------------------------------------- the doubles

/// A host that keeps services and, unlike `ServiceHost`, is somewhere a notice
/// can land: the one drain says a notice through the host rather than through
/// a call, so this is where a test reads what the bridge said. Everything else
/// is the absent host's.
pub struct Listening {
    services: HostHandle,
    said: Mutex<Vec<(Level, String, String)>>,
}

impl Listening {
    pub fn new() -> Self {
        Self {
            services: bingo_sdk::testing::ServiceHost::handle(),
            said: Mutex::new(Vec::new()),
        }
    }

    /// The level and text of the first notice filed under this code.
    pub fn heard(&self, code: &str) -> Option<(Level, String)> {
        self.said
            .lock()
            .unwrap()
            .iter()
            .find(|(_, said, _)| said == code)
            .map(|(level, _, text)| (*level, text.clone()))
    }

    pub fn all(&self) -> Vec<(Level, String, String)> {
        self.said.lock().unwrap().clone()
    }
}

#[async_trait]
impl HostApi for Listening {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        self.services.0.sessions(filter).await
    }

    async fn open(
        &self,
        selector: SessionSelector,
        who: ClientIdentity,
        options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        self.services.0.open(selector, who, options).await
    }

    async fn close(&self, session: &SessionId, reason: CloseReason) -> Result<(), KernelError> {
        self.services.0.close(session, reason).await
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        self.services.0.delete(session).await
    }

    async fn deliver(
        &self,
        to: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        self.services.0.deliver(to, intent, input, delivery).await
    }

    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.services.0.extend(session, plugin, kind, payload).await
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.services.0.signal(session, plugin, kind, payload).await
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        self.services.0.catalog(kind).await
    }

    async fn notice(&self, level: Level, code: &str, text: &str) -> Result<(), KernelError> {
        self.said
            .lock()
            .unwrap()
            .push((level, code.to_string(), text.to_string()));
        Ok(())
    }

    fn gateway_events(&self) -> GatewayStream {
        self.services.0.gateway_events()
    }

    fn service_any(&self, key: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.services.0.service_any(key)
    }

    fn service_wire(&self, key: &str) -> Option<Arc<dyn WireService>> {
        self.services.0.service_wire(key)
    }

    fn open_service(&self, key: &str, wire: Arc<dyn WireService>) -> Result<(), KernelError> {
        self.services.0.open_service(key, wire)
    }
}

/// A tool host that keeps what a call told it, so a test can read the live
/// output line and the notices the transcript was given — and answers the
/// questions the call asks with whatever it was built with.
#[derive(Debug)]
pub struct Recorder {
    progress: Mutex<Vec<String>>,
    recorded: Mutex<Vec<ItemBody>>,
    asked: Mutex<Vec<InteractionKind>>,
    answer: Answer,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder::answering(Answer::Cancel)
    }
}

impl Recorder {
    /// A person who always answers this way.
    pub fn answering(answer: Answer) -> Self {
        Self {
            progress: Mutex::new(Vec::new()),
            recorded: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
            answer,
        }
    }

    pub fn progress(&self) -> Vec<String> {
        self.progress.lock().unwrap().clone()
    }

    pub fn recorded(&self) -> Vec<ItemBody> {
        self.recorded.lock().unwrap().clone()
    }

    /// What this call was asked, in the order it was asked.
    pub fn asked(&self) -> Vec<InteractionKind> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl Prompter for Recorder {
    async fn ask(
        &self,
        kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.asked.lock().unwrap().push(kind);
        Ok(self.answer.clone())
    }
}

#[async_trait]
impl ToolHost for Recorder {
    fn progress(&self, _item: &ItemId, tail: String) {
        self.progress.lock().unwrap().push(tail);
    }

    async fn record(&self, body: ItemBody) -> Result<ItemId, KernelError> {
        self.recorded.lock().unwrap().push(body);
        Ok(ItemId::from_raw("itm_test"))
    }
}

/// Where a hook is standing, for a test that drives one itself: a session, a
/// turn, a directory, and nothing that answers.
pub fn hook_context() -> bingo_sdk::HookContext {
    bingo_sdk::HookContext {
        session: SessionId::from_raw("ses_test"),
        turn: Some(TurnId::from_raw("trn_test")),
        cwd: PathBuf::from("/work"),
        provider: None,
        model: Some("stub-1".into()),
        host: bingo_sdk::testing::NoHost::handle(),
    }
}

pub fn context(call: Arc<Recorder>, cwd: &Path, cancel: CancellationToken) -> ToolContext {
    ToolContext {
        call_id: CALL_ID.into(),
        session: SessionId::from_raw("ses_test"),
        turn: TurnId::from_raw("trn_test"),
        item: ItemId::from_raw("itm_test"),
        cwd: cwd.to_path_buf(),
        cancel,
        env: Arc::new(Env::rooted("/nowhere")),
        host: bingo_sdk::testing::NoHost::handle(),
        call,
    }
}

pub async fn call(
    tool: &Arc<dyn Tool>,
    input: Value,
    cwd: &Path,
) -> (Arc<Recorder>, Result<ToolOutput, ToolError>) {
    calling(Arc::new(Recorder::default()), tool, input, cwd).await
}

/// The same, with the person the call reaches decided by the test: what a
/// question put through the running call comes back as.
pub async fn calling(
    recorder: Arc<Recorder>,
    tool: &Arc<dyn Tool>,
    input: Value,
    cwd: &Path,
) -> (Arc<Recorder>, Result<ToolOutput, ToolError>) {
    let cx = context(Arc::clone(&recorder), cwd, CancellationToken::new());
    let answered = tool.call(input, &cx).await;
    (recorder, answered)
}

/// The id every call this harness makes is filed under: `bingo.host.ask`
/// names it, so a test writing an ask has to know it.
pub const CALL_ID: &str = "call_test";

pub fn said(output: &ToolOutput) -> String {
    output.parts[0].as_text().unwrap_or_default().to_string()
}
