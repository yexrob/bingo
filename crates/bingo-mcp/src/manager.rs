//! What every configured server is doing, and the dialling that moves it on.
//!
//! The configured set never changes, so it is held without a lock; only what a
//! server is *doing* is shared, behind one `RwLock` that is taken to claim a
//! dial and taken again to file its outcome — never held while a handshake is
//! in flight. Ten servers therefore cost the slowest one, not the sum, and a
//! turn that assembles its tools mid-handshake sees the servers that have
//! landed instead of waiting for the ones that have not (ADR-0009 §1).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bingo_auth_oauth::{CredentialStore, McpAuth};
use bingo_sdk::Tool;
use tokio::sync::RwLock;

use crate::config::Server;
use crate::dial::{self, Connection, Dialled};
use crate::tool::McpTool;

/// What one server is doing. There is no sixth thing: a server is on its way,
/// answering, waiting for a person to sign in, out of action with a reason, or
/// switched off.
enum State {
    Connecting,
    Connected(Box<Connection>),
    /// The server answered `401` and this run holds no credential it accepts
    /// (ADR-0050 §3). Not a failure to retry: a sign-in nobody has done.
    NeedsAuth {
        why: String,
    },
    Failed {
        why: String,
    },
    Disabled,
}

/// A server's state and the dial it belongs to. A handshake that lands after
/// the server was disabled or dialled again belongs to an epoch that has
/// passed, and is dropped rather than filed.
struct Slot {
    epoch: u64,
    state: State,
}

/// What `/mcp` says about a server: the state with the live connection left
/// behind, so no view ever holds one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected { tools: usize },
    NeedsAuth { why: String },
    Failed { why: String },
    Disabled,
}

/// One line of the `/mcp` table: what a server is doing, and how it stands
/// with the sign-in it may need. The two are different facts — a server can be
/// signed in to and still unreachable — so they are two columns, not one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub server: String,
    pub status: Status,
    /// `None` for a server that signs in to nothing: a child process, or an
    /// HTTP server whose `Authorization` a person wrote themselves.
    pub auth: Option<bingo_auth_oauth::Status>,
}

pub struct Manager {
    servers: BTreeMap<String, Server>,
    /// One per server that can be signed in to, fixed at construction as the
    /// servers themselves are.
    auth: BTreeMap<String, Arc<McpAuth>>,
    data_dir: PathBuf,
    slots: RwLock<BTreeMap<String, Slot>>,
}

/// What a server is doing is behind an async lock and cannot be read here, so
/// a manager prints the set it was configured with and nothing else.
impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("servers", &self.servers.keys())
            .finish_non_exhaustive()
    }
}

impl Manager {
    pub fn new(servers: BTreeMap<String, Server>, disabled: &[String], data_dir: PathBuf) -> Self {
        let slots = servers
            .keys()
            .map(|name| {
                let state = if disabled.iter().any(|off| off == name) {
                    State::Disabled
                } else {
                    State::Connecting
                };
                (name.clone(), Slot { epoch: 0, state })
            })
            .collect();
        let auth = crate::auth::handles(&servers, Arc::new(CredentialStore::new(data_dir.clone())));
        Self {
            servers,
            auth,
            data_dir,
            slots: RwLock::new(slots),
        }
    }

    /// How this server signs in, for the verbs that do it. `None` when it
    /// signs in to nothing.
    pub fn auth(&self, name: &str) -> Option<&Arc<McpAuth>> {
        self.auth.get(name)
    }

    /// Whether this name was configured at all.
    pub fn knows(&self, name: &str) -> bool {
        self.servers.contains_key(name)
    }

    /// Every configured server's name, in the order a person reads them.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.servers.keys().map(String::as_str)
    }

    /// The tools of every connected server, as they stand now. Dials nothing:
    /// a turn asking for its tool set must never wait on a handshake.
    /// Each tool holds a weak way back here, because a call this server
    /// refuses with a `401` mid-session is what starts the redial (ADR-0050
    /// §3) — the tool learns it happened, the manager owns what to do.
    pub async fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        let slots = self.slots.read().await;
        slots
            .iter()
            .flat_map(|(name, slot)| offered(name, &slot.state, Arc::downgrade(self)))
            .collect()
    }

    /// A call this server refused with a `401` while the session was running.
    /// Claimed from a live connection only, so a burst of refused calls starts
    /// one redial and not one each; the dial itself renews and tries again,
    /// and files *needs authentication* when that fails too.
    pub async fn signed_out(self: &Arc<Self>, name: &str) -> bool {
        if !self.auth.contains_key(name) {
            return false;
        }
        let claimed = {
            let mut slots = self.slots.write().await;
            match slots.get_mut(name) {
                Some(slot) if matches!(slot.state, State::Connected(_)) => Some(begin(slot)),
                _ => None,
            }
        };
        self.spawn_dial(name, claimed)
    }

    /// The rows of every server a person has left switched on, keyed by name
    /// and shaped the way they wrote them.
    ///
    /// Not the tools: whoever asks for these is going to dial the servers
    /// itself, so what it needs is the row (ADR-0036 §4). A server that is
    /// switched off is not a row anyone should be handed — bingo would not
    /// dial it either.
    pub async fn rows(&self) -> serde_json::Map<String, serde_json::Value> {
        let slots = self.slots.read().await;
        self.servers
            .iter()
            .filter(|(name, _)| !matches!(slots.get(*name).map(off), Some(true)))
            .map(|(name, server)| (name.clone(), crate::config::row(server)))
            .collect()
    }

    /// One line per configured server, for `/mcp`.
    pub async fn lines(&self) -> Vec<Line> {
        let slots = self.slots.read().await;
        slots
            .iter()
            .map(|(name, slot)| Line {
                server: name.clone(),
                status: status_of(&slot.state),
                auth: self.auth.get(name).map(|auth| auth.status()),
            })
            .collect()
    }

    /// What one connected server said it can do, for `/mcp tools`.
    pub async fn tools_of(&self, name: &str) -> Option<Vec<(String, String)>> {
        let slots = self.slots.read().await;
        let State::Connected(connection) = &slots.get(name)?.state else {
            return None;
        };
        Some(
            connection
                .tools
                .iter()
                .map(|tool| {
                    let described = tool.description.as_deref().unwrap_or_default();
                    (tool.name.to_string(), first_line(described))
                })
                .collect(),
        )
    }

    /// Dial every server that is waiting for one, all at once. Returns when
    /// the last of them has landed; `start` spawns this and returns at once.
    pub async fn dial_enabled(self: &Arc<Self>) {
        let mut running = tokio::task::JoinSet::new();
        for (name, epoch) in self.pending().await {
            let manager = Arc::clone(self);
            running.spawn(async move { manager.dial_one(name, epoch).await });
        }
        while running.join_next().await.is_some() {}
    }

    /// Dial the server again, dropping what it has. `false` when nothing began
    /// because the server is switched off.
    pub async fn reconnect(self: &Arc<Self>, name: &str) -> bool {
        let claimed = {
            let mut slots = self.slots.write().await;
            match slots.get_mut(name) {
                Some(slot) if !matches!(slot.state, State::Disabled) => Some(begin(slot)),
                _ => None,
            }
        };
        self.spawn_dial(name, claimed)
    }

    /// Switch the server back on and dial it. `false` when it was already on.
    pub async fn enable(self: &Arc<Self>, name: &str) -> bool {
        let claimed = {
            let mut slots = self.slots.write().await;
            match slots.get_mut(name) {
                Some(slot) if matches!(slot.state, State::Disabled) => Some(begin(slot)),
                _ => None,
            }
        };
        self.spawn_dial(name, claimed)
    }

    /// Switch the server off and drop its connection. `false` when it was
    /// already off. A call already running keeps its own handle on the
    /// connection and finishes.
    pub async fn disable(&self, name: &str) -> bool {
        let mut slots = self.slots.write().await;
        let Some(slot) = slots.get_mut(name) else {
            return false;
        };
        if matches!(slot.state, State::Disabled) {
            return false;
        }
        slot.epoch += 1;
        slot.state = State::Disabled;
        true
    }

    /// Drop every connection. The host is closing, so no server is on its way
    /// any more and none offers tools; the epoch bump keeps a handshake still
    /// in flight from filing itself afterwards.
    pub async fn shutdown(&self) {
        let mut slots = self.slots.write().await;
        for slot in slots.values_mut() {
            slot.epoch += 1;
            slot.state = State::Disabled;
        }
    }

    fn spawn_dial(self: &Arc<Self>, name: &str, claimed: Option<u64>) -> bool {
        let Some(epoch) = claimed else {
            return false;
        };
        let manager = Arc::clone(self);
        let name = name.to_string();
        tokio::spawn(async move { manager.dial_one(name, epoch).await });
        true
    }

    /// The servers on their way, claimed under the lock so that the dial that
    /// follows holds nothing.
    async fn pending(&self) -> Vec<(String, u64)> {
        let slots = self.slots.read().await;
        slots
            .iter()
            .filter(|(_, slot)| matches!(slot.state, State::Connecting))
            .map(|(name, slot)| (name.clone(), slot.epoch))
            .collect()
    }

    async fn dial_one(&self, name: String, epoch: u64) {
        let Some(server) = self.servers.get(&name) else {
            return;
        };
        let outcome = self.attempt(&name, server).await;
        report(&name, &outcome);
        self.file(&name, epoch, outcome).await;
    }

    /// One dial with the bearer the store has, and — when that bearer is what
    /// the server refused — one more with a renewed one (ADR-0050 §3).
    async fn attempt(&self, name: &str, server: &Server) -> Dialled {
        let auth = self.auth.get(name);
        let bearer = match auth {
            Some(auth) => auth.access_token().await.ok(),
            None => None,
        };
        let dialled = dial::dial(name, server, &self.data_dir, bearer.as_deref()).await;
        let Dialled::Unauthorized { why } = dialled else {
            return dialled;
        };
        let Some(auth) = auth else {
            // A person's own `Authorization` was refused: theirs to fix, and
            // no sign-in bingo could offer would replace it.
            return Dialled::Failed { why };
        };
        match &bearer {
            Some(refused) => self.renewed(name, server, auth, refused, why).await,
            None => Dialled::Unauthorized { why },
        }
    }

    /// The one renewal a refused dial gets. `refused` is the bearer the server
    /// would not take, which is what keeps this single flight; `why` is the
    /// server's own words, already redacted, and the only thing a person is
    /// shown when the renewal fails too.
    async fn renewed(
        &self,
        name: &str,
        server: &Server,
        auth: &McpAuth,
        refused: &str,
        why: String,
    ) -> Dialled {
        let Ok(bearer) = auth.refreshed(refused).await else {
            return Dialled::Unauthorized { why };
        };
        dial::dial(name, server, &self.data_dir, Some(&bearer)).await
    }

    async fn file(&self, name: &str, epoch: u64, outcome: Dialled) {
        let mut slots = self.slots.write().await;
        let Some(slot) = slots.get_mut(name) else {
            return;
        };
        if slot.epoch != epoch {
            return;
        }
        slot.state = match outcome {
            Dialled::Connected(connection) => State::Connected(connection),
            Dialled::Unauthorized { why } => State::NeedsAuth { why },
            Dialled::Failed { why } => State::Failed { why },
        };
    }
}

/// Claim a fresh dial of this server: what it had is gone and what is in
/// flight for it no longer counts.
fn begin(slot: &mut Slot) -> u64 {
    slot.epoch += 1;
    slot.state = State::Connecting;
    slot.epoch
}

/// One line per outcome, so a server that never arrives says so somewhere a
/// person can read it. The reason is the transport's; nothing of the
/// configuration is printed.
fn report(name: &str, outcome: &Dialled) {
    match outcome {
        Dialled::Connected(connection) => tracing::info!(
            server = name,
            tools = connection.tools.len(),
            "mcp server connected"
        ),
        Dialled::Unauthorized { why } => {
            tracing::info!(server = name, %why, "mcp server needs authentication")
        }
        Dialled::Failed { why } => tracing::warn!(server = name, %why, "mcp server unavailable"),
    }
}

fn offered(server: &str, state: &State, manager: std::sync::Weak<Manager>) -> Vec<Arc<dyn Tool>> {
    let State::Connected(connection) = state else {
        return Vec::new();
    };
    connection
        .tools
        .iter()
        .map(|listed| {
            Arc::new(McpTool::new(
                server,
                listed,
                Arc::clone(&connection.service),
                Arc::clone(&connection.asker),
                manager.clone(),
            )) as Arc<dyn Tool>
        })
        .collect()
}

/// A tool's description as a table can hold it.
fn first_line(description: &str) -> String {
    description
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Whether a person has switched this server off.
fn off(slot: &Slot) -> bool {
    matches!(slot.state, State::Disabled)
}

fn status_of(state: &State) -> Status {
    match state {
        State::Connecting => Status::Connecting,
        State::Connected(connection) => Status::Connected {
            tools: connection.tools.len(),
        },
        State::NeedsAuth { why } => Status::NeedsAuth { why: why.clone() },
        State::Failed { why } => Status::Failed { why: why.clone() },
        State::Disabled => Status::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str) -> Server {
        Server::Stdio {
            command: command.to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    fn manager(disabled: &[String]) -> Arc<Manager> {
        let servers = BTreeMap::from([
            ("files".to_string(), stdio("/bin/echo")),
            ("web".to_string(), stdio("/bin/echo")),
        ]);
        Arc::new(Manager::new(
            servers,
            disabled,
            std::env::temp_dir().join("bingo-mcp-manager-tests"),
        ))
    }

    /// The states of every configured server, in the order they are read.
    async fn states(manager: &Manager) -> Vec<(String, Status)> {
        manager
            .lines()
            .await
            .into_iter()
            .map(|line| (line.server, line.status))
            .collect()
    }

    #[tokio::test]
    async fn a_configured_server_is_on_its_way_before_anything_is_dialled() {
        let manager = manager(&[]);
        assert_eq!(
            states(&manager).await,
            vec![
                ("files".to_string(), Status::Connecting),
                ("web".to_string(), Status::Connecting),
            ]
        );
        assert!(manager.tools().await.is_empty());
    }

    /// A child process signs in to nothing, so its line has no auth column
    /// to fill (ADR-0050 §3).
    #[tokio::test]
    async fn a_child_process_has_no_sign_in_of_its_own() {
        let manager = manager(&[]);
        assert!(manager.lines().await.iter().all(|line| line.auth.is_none()));
        assert!(manager.auth("files").is_none());
    }

    #[tokio::test]
    async fn a_disabled_server_starts_switched_off_and_is_never_dialled() {
        let manager = manager(&["web".to_string()]);
        assert_eq!(
            states(&manager).await,
            vec![
                ("files".to_string(), Status::Connecting),
                ("web".to_string(), Status::Disabled),
            ]
        );
        assert_eq!(manager.pending().await, vec![("files".to_string(), 0)]);
    }

    #[tokio::test]
    async fn only_a_configured_name_is_known() {
        let manager = manager(&[]);
        assert!(manager.knows("files"));
        assert!(!manager.knows("nothing"));
        assert_eq!(manager.names().collect::<Vec<_>>(), ["files", "web"]);
    }

    #[tokio::test]
    async fn switching_a_server_off_and_on_says_what_changed() {
        let manager = manager(&[]);
        assert!(manager.disable("files").await);
        assert!(!manager.disable("files").await, "already off");
        assert!(!manager.disable("nothing").await, "never configured");
        assert!(manager.enable("files").await);
        assert!(!manager.enable("files").await, "already on");
    }

    #[tokio::test]
    async fn a_switched_off_server_is_not_reconnected() {
        let manager = manager(&["web".to_string()]);
        assert!(!manager.reconnect("web").await);
        assert_eq!(states(&manager).await[1].1, Status::Disabled);
    }

    /// The race the epoch exists for: a handshake that lands after a person
    /// switched the server off must not switch it back on.
    #[tokio::test]
    async fn a_handshake_that_lands_after_a_disable_is_dropped() {
        let manager = manager(&[]);
        let epoch = manager.pending().await[0].1;
        manager.disable("files").await;
        manager
            .file(
                "files",
                epoch,
                Dialled::Failed {
                    why: "too late".to_string(),
                },
            )
            .await;
        assert_eq!(states(&manager).await[0].1, Status::Disabled);
    }

    #[tokio::test]
    async fn a_handshake_of_the_current_dial_is_filed() {
        let manager = manager(&[]);
        let epoch = manager.pending().await[0].1;
        manager
            .file(
                "files",
                epoch,
                Dialled::Failed {
                    why: "no such command".to_string(),
                },
            )
            .await;
        assert_eq!(
            states(&manager).await[0].1,
            Status::Failed {
                why: "no such command".to_string()
            }
        );
    }

    /// ADR-0050 §3: a `401` a person can do something about is filed as a
    /// sign-in they have not done, not as a failure they should retry.
    #[tokio::test]
    async fn a_server_that_answered_401_is_filed_as_needing_authentication() {
        let manager = manager(&[]);
        let epoch = manager.pending().await[0].1;
        manager
            .file(
                "files",
                epoch,
                Dialled::Unauthorized {
                    why: "handshake: Auth required".to_string(),
                },
            )
            .await;
        assert_eq!(
            states(&manager).await[0].1,
            Status::NeedsAuth {
                why: "handshake: Auth required".to_string()
            }
        );
        assert!(
            manager.tools().await.is_empty(),
            "a server nobody signed in to offers nothing"
        );
    }

    /// The redial is claimed from a live connection only, so a burst of
    /// refused calls starts one and not one each.
    #[tokio::test]
    async fn a_mid_session_401_on_a_server_with_no_sign_in_starts_nothing() {
        let manager = manager(&[]);
        assert!(!manager.signed_out("files").await);
        assert!(!manager.signed_out("nothing").await);
    }

    #[tokio::test]
    async fn shutting_down_leaves_no_server_offering_tools() {
        let manager = manager(&[]);
        manager.shutdown().await;
        assert!(manager.tools().await.is_empty());
        assert!(manager.pending().await.is_empty());
    }
}
