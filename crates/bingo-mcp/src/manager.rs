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

use bingo_sdk::Tool;
use tokio::sync::RwLock;

use crate::config::Server;
use crate::dial::{self, Connection};
use crate::tool::McpTool;

/// What one server is doing. There is no fifth thing: a server is on its way,
/// answering, out of action with a reason, or switched off.
enum State {
    Connecting,
    Connected(Connection),
    Failed { why: String },
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
    Failed { why: String },
    Disabled,
}

pub struct Manager {
    servers: BTreeMap<String, Server>,
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
        Self {
            servers,
            data_dir,
            slots: RwLock::new(slots),
        }
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
    pub async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let slots = self.slots.read().await;
        slots
            .iter()
            .flat_map(|(name, slot)| tools_of(name, &slot.state))
            .collect()
    }

    /// One line per configured server, for `/mcp`.
    pub async fn statuses(&self) -> Vec<(String, Status)> {
        let slots = self.slots.read().await;
        slots
            .iter()
            .map(|(name, slot)| (name.clone(), status_of(&slot.state)))
            .collect()
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
        let outcome = dial::dial(&name, server, &self.data_dir).await;
        report(&name, &outcome);
        self.file(&name, epoch, outcome).await;
    }

    async fn file(&self, name: &str, epoch: u64, outcome: Result<Connection, String>) {
        let mut slots = self.slots.write().await;
        let Some(slot) = slots.get_mut(name) else {
            return;
        };
        if slot.epoch != epoch {
            return;
        }
        slot.state = match outcome {
            Ok(connection) => State::Connected(connection),
            Err(why) => State::Failed { why },
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
fn report(name: &str, outcome: &Result<Connection, String>) {
    match outcome {
        Ok(connection) => tracing::info!(
            server = name,
            tools = connection.tools.len(),
            "mcp server connected"
        ),
        Err(why) => tracing::warn!(server = name, %why, "mcp server unavailable"),
    }
}

fn tools_of(server: &str, state: &State) -> Vec<Arc<dyn Tool>> {
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
            )) as Arc<dyn Tool>
        })
        .collect()
}

fn status_of(state: &State) -> Status {
    match state {
        State::Connecting => Status::Connecting,
        State::Connected(connection) => Status::Connected {
            tools: connection.tools.len(),
        },
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

    #[tokio::test]
    async fn a_configured_server_is_on_its_way_before_anything_is_dialled() {
        let manager = manager(&[]);
        assert_eq!(
            manager.statuses().await,
            vec![
                ("files".to_string(), Status::Connecting),
                ("web".to_string(), Status::Connecting),
            ]
        );
        assert!(manager.tools().await.is_empty());
    }

    #[tokio::test]
    async fn a_disabled_server_starts_switched_off_and_is_never_dialled() {
        let manager = manager(&["web".to_string()]);
        assert_eq!(
            manager.statuses().await,
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
        assert_eq!(manager.statuses().await[1].1, Status::Disabled);
    }

    /// The race the epoch exists for: a handshake that lands after a person
    /// switched the server off must not switch it back on.
    #[tokio::test]
    async fn a_handshake_that_lands_after_a_disable_is_dropped() {
        let manager = manager(&[]);
        let epoch = manager.pending().await[0].1;
        manager.disable("files").await;
        manager
            .file("files", epoch, Err("too late".to_string()))
            .await;
        assert_eq!(manager.statuses().await[0].1, Status::Disabled);
    }

    #[tokio::test]
    async fn a_handshake_of_the_current_dial_is_filed() {
        let manager = manager(&[]);
        let epoch = manager.pending().await[0].1;
        manager
            .file("files", epoch, Err("no such command".to_string()))
            .await;
        assert_eq!(
            manager.statuses().await[0].1,
            Status::Failed {
                why: "no such command".to_string()
            }
        );
    }

    #[tokio::test]
    async fn shutting_down_leaves_no_server_offering_tools() {
        let manager = manager(&[]);
        manager.shutdown().await;
        assert!(manager.tools().await.is_empty());
        assert!(manager.pending().await.is_empty());
    }
}
