//! What the tests in this crate share: a home on disk with a store over it,
//! the context the sdk hands a tool, and the host that writes down what it
//! was told.
//!
//! A tool over this store writes files, rings a bell and publishes what
//! stands; one that asked the kernel for anything else fails loudly here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, CancellationToken, CommandContext, Delivery, Env, HostApi, HostHandle,
    Input, IntentId, InteractionKind, ItemBody, ItemId, KernelError, Prompter, SessionId,
    ToolContext, ToolHost, ToolOutput, TurnId, testing::NoHost,
};
use serde_json::Value;

use crate::schedules::Schedules;
use crate::store::{Shelf, Store};

pub(crate) struct Fixture {
    home: tempfile::TempDir,
    pub(crate) schedules: Arc<Schedules>,
    pub(crate) host: Arc<Listening>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().expect("a temp home");
        let env = Env::rooted(home.path());
        let schedules = Arc::new(Schedules::new(&env.data_dir));
        Self {
            home,
            schedules,
            host: Arc::new(Listening::default()),
        }
    }

    pub(crate) fn handle(&self) -> HostHandle {
        HostHandle(self.host.clone())
    }

    pub(crate) fn cwd(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    pub(crate) fn shelf(&self) -> Shelf {
        self.schedules.store().load()
    }

    /// A store of its own over the same directory: what the runner is handed.
    pub(crate) fn store(&self) -> Arc<Store> {
        Arc::new(Store::new(&Env::rooted(self.home.path()).data_dir))
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
            host: self.handle(),
            call: Arc::new(Silent),
        }
    }

    pub(crate) fn command(&self) -> CommandContext {
        CommandContext {
            session: SessionId::from_raw("ses_test"),
            cwd: self.cwd(),
            host: self.handle(),
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

/// A host that writes down what it was told and answers everything else the
/// way [`NoHost`] does: what a wake reaches for is a delivery and a published
/// kind, and both are read back here.
#[derive(Debug, Default)]
pub(crate) struct Listening {
    delivered: Mutex<Vec<(SessionId, Input, Delivery)>>,
    extended: Mutex<Vec<(SessionId, String, String, Value)>>,
}

impl Listening {
    pub(crate) fn delivered(&self) -> Vec<(SessionId, Input, Delivery)> {
        self.delivered
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    pub(crate) fn extended(&self) -> Vec<(SessionId, String, String, Value)> {
        self.extended
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }
}

#[async_trait]
impl HostApi for Listening {
    async fn sessions(
        &self,
        filter: bingo_sdk::SessionFilter,
    ) -> Result<Vec<bingo_sdk::SessionSummary>, KernelError> {
        NoHost.sessions(filter).await
    }

    async fn open(
        &self,
        selector: bingo_sdk::SessionSelector,
        who: bingo_sdk::ClientIdentity,
        options: bingo_sdk::OpenOptions,
    ) -> Result<bingo_sdk::Attachment, KernelError> {
        NoHost.open(selector, who, options).await
    }

    async fn close(
        &self,
        session: &SessionId,
        reason: bingo_sdk::CloseReason,
    ) -> Result<(), KernelError> {
        NoHost.close(session, reason).await
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        NoHost.delete(session).await
    }

    async fn deliver(
        &self,
        to: &SessionId,
        _intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        self.delivered
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push((to.clone(), input, delivery));
        Ok(())
    }

    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.extended
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push((
                session.clone(),
                plugin.to_string(),
                kind.to_string(),
                payload,
            ));
        Ok(())
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        NoHost.signal(session, plugin, kind, payload).await
    }

    async fn catalog(
        &self,
        kind: bingo_sdk::CatalogKind,
    ) -> Result<bingo_sdk::Catalog, KernelError> {
        NoHost.catalog(kind).await
    }

    fn gateway_events(&self) -> bingo_sdk::GatewayStream {
        NoHost.gateway_events()
    }

    fn service_any(&self, key: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        NoHost.service_any(key)
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
