//! What the integration tests share: the scripted fake agent as a real child
//! process, spoken to over real pipes. Every wait is bounded, so a scenario
//! that stalls fails instead of hanging the suite.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out. Each test binary uses a slice of this module.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol_schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, Error as RpcError,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use async_trait::async_trait;
use bingo_provider_acp::child::{self, Spawned};
use bingo_provider_acp::connection::{Client, Connection};
use bingo_provider_acp::refusal;
use serde_json::Value;
use tokio::sync::Mutex;

/// How long any one wait may take before the scenario is called stalled. CI is
/// slower than a developer's box, so this is generous rather than tight.
pub const LIMIT: Duration = Duration::from_secs(20);

pub fn agent_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bingo-fake-acp-agent"))
}

/// A scripted agent on disk: the script, the log it appends to, and the
/// directory both live in.
pub struct Fake {
    pub home: tempfile::TempDir,
    pub script: PathBuf,
    pub log: PathBuf,
}

impl Fake {
    pub fn new(script: Value) -> Self {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("acp-script.json");
        std::fs::write(&path, script.to_string()).unwrap();
        let log = home.path().join("acp-log.jsonl");
        Fake {
            home,
            script: path,
            log,
        }
    }

    /// What the agent obeys from the next spawn on. A scenario that must prove
    /// nothing spawned again says so by putting a different script where the
    /// next child would read one.
    pub fn rewrite(&self, script: Value) {
        std::fs::write(&self.script, script.to_string()).unwrap();
    }

    pub fn env(&self) -> std::collections::BTreeMap<String, String> {
        [
            (
                "BINGO_FAKE_ACP_SCRIPT".to_string(),
                self.script.display().to_string(),
            ),
            (
                "BINGO_FAKE_ACP_LOG".to_string(),
                self.log.display().to_string(),
            ),
        ]
        .into_iter()
        .collect()
    }

    pub fn cwd(&self) -> &Path {
        self.home.path()
    }

    /// Every message the agent received, in order.
    pub fn heard(&self) -> Vec<Value> {
        let Ok(body) = std::fs::read_to_string(&self.log) else {
            return Vec::new();
        };
        body.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn methods(&self) -> Vec<String> {
        self.heard()
            .into_iter()
            .map(|line| line["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The params of the first message with this method, if it arrived.
    pub fn first(&self, method: &str) -> Option<Value> {
        self.heard()
            .into_iter()
            .find(|line| line["method"] == method)
            .map(|line| line["params"].clone())
    }

    /// Wait until the agent has recorded `method`, or fail the scenario.
    pub async fn wait_for(&self, method: &str) -> Value {
        let deadline = tokio::time::Instant::now() + LIMIT;
        loop {
            if let Some(params) = self.first(method) {
                return params;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "the agent never heard {method}; it heard {:?}",
                    self.methods()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// A client that collects the stream and answers a question the way the plugin
/// does when there is nobody behind it to ask: with the agent's own refusal
/// (ADR-0039 §3). What a session with somebody at it does instead is a
/// scenario about a person, not about this wire, and is black-box.
pub struct Collector {
    pub updates: Arc<Mutex<Vec<SessionNotification>>>,
}

impl Collector {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            updates: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait]
impl Client for Collector {
    async fn update(&self, notification: SessionNotification) {
        self.updates.lock().await.push(notification);
    }

    async fn permission(
        &self,
        request: RequestPermissionRequest,
    ) -> Result<RequestPermissionResponse, RpcError> {
        Ok(refusal::refused(&request))
    }

    async fn elicitation(
        &self,
        _request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, RpcError> {
        Ok(refusal::declined())
    }
}

/// The whole apparatus: a spawned agent and a connection to it. The adapter
/// handle is kept so the tree dies with the test.
pub struct Live {
    pub connection: Connection,
    pub adapter: child::Adapter,
}

pub fn connect(fake: &Fake, client: Arc<dyn Client>) -> Live {
    let Spawned {
        adapter,
        reader,
        writer,
    } = child::spawn(
        &agent_binary().display().to_string(),
        &[],
        &fake.env(),
        fake.cwd(),
    )
    .expect("the fake agent starts");
    Live {
        connection: Connection::spawn(reader, writer, client),
        adapter,
    }
}
