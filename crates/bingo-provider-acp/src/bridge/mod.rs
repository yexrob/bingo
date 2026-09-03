//! The tool bridge: bingo's shared tools, served to an ACP agent as MCP.
//!
//! ADR-0036: the agent brings its own hands but has no way to speak into the
//! house, and the protocol left the door open — `session/new.mcpServers` hands
//! it servers to dial, and stdio is the one transport every agent must speak.
//! So one rendezvous per run ([`socket`]), one [`token`] per ACP session, a
//! [`handshake`] that says which conversation a stream is, and `rmcp`'s server
//! half on what was accepted ([`serve`]).
//!
//! The pieces below the transport are two: [`doors`] is the seam into bingo —
//! the offer and the call — and [`shape`] is the pure translation between what
//! a tool is here and what it is on the wire. Nothing in this module knows
//! what a turn is.
//!
//! A bridge ends when it is dropped, the way everything else in this crate
//! ends: dropping it stops the accept loop, and the listener that goes with it
//! takes the socket.

pub mod doors;
pub mod handshake;
pub mod serve;
pub mod shape;
pub mod socket;
pub mod token;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bingo_sdk::Env;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::error::AcpError;

pub use doors::{Doors, Refused};
pub use socket::Address;
pub use token::Token;

/// Where the proxy dials. Read by `acp-mcp-proxy`, written into the server
/// row's `env` — never its argv, which is world-readable (ADR-0036 §3).
pub const ADDRESS_VAR: &str = "BINGO_ACP_BRIDGE_ADDRESS";

/// Which conversation the proxy is. Same reason, and more of it.
pub const TOKEN_VAR: &str = "BINGO_ACP_BRIDGE_TOKEN";

/// One run's bridge. Dropping it is its stop.
pub struct Bridge {
    address: Address,
    conversations: Arc<Conversations>,
    accepting: JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

impl Bridge {
    /// Listen on this run's own address.
    pub fn open(env: &Env) -> Result<Self, AcpError> {
        Self::at(Address::of_run(env, std::process::id()))
    }

    /// Listen on an address given rather than derived. The tests' door, and
    /// the one place the two halves of [`Address`] meet.
    pub fn at(address: Address) -> Result<Self, AcpError> {
        let listener = socket::Listener::bind(&address)?;
        let conversations = Arc::new(Conversations::default());
        let accepting = tokio::spawn(accepting(listener, conversations.clone()));
        Ok(Self {
            address,
            conversations,
            accepting,
        })
    }

    /// Where a server row must point.
    pub fn address(&self) -> &Address {
        &self.address
    }

    /// Open a conversation for one ACP session: the token that names it and
    /// the doors its calls go through. The token is what rides the row's env.
    pub fn admit(&self, doors: Arc<dyn Doors>) -> Result<Token, AcpError> {
        let token = Token::mint()?;
        self.conversations.add(token.clone(), doors);
        Ok(token)
    }

    /// Forget a conversation. A session that has ended has no tools to offer,
    /// and its token must stop being an address.
    pub fn dismiss(&self, token: &Token) {
        self.conversations.remove(token);
    }

    /// The offer moved: every live conversation hears `tools/list_changed`.
    /// A conversation that is not dialled yet needs no telling — it will ask.
    pub fn offer_changed(&self) {
        self.conversations.announce();
    }
}

/// Every conversation this bridge has opened.
///
/// A `std::sync::Mutex`, not tokio's: nothing here awaits while it is held,
/// and the accept path must be able to release a token from a `Drop`.
#[derive(Default)]
struct Conversations {
    live: Mutex<Vec<Conversation>>,
    serial: AtomicU64,
}

struct Conversation {
    token: Token,
    doors: Arc<dyn Doors>,
    changed: Arc<Notify>,
    /// Set while a stream is being served on this token. A second *concurrent*
    /// stream is refused — one conversation is one agent — but a token whose
    /// stream has closed may be dialled again: an agent's MCP client may
    /// respawn a proxy that died and re-dial mid-session, and a session that
    /// could not be rejoined would be mute for the rest of its life.
    serving: bool,
}

impl Conversations {
    fn add(&self, token: Token, doors: Arc<dyn Doors>) {
        self.lock().push(Conversation {
            token,
            doors,
            changed: Arc::new(Notify::new()),
            serving: false,
        });
    }

    fn remove(&self, token: &Token) {
        self.lock().retain(|open| &open.token != token);
    }

    fn announce(&self) {
        for open in self.lock().iter() {
            open.changed.notify_one();
        }
    }

    /// Take the conversation this stream claims to be, if the token names one
    /// that is not already being served. Scanned rather than looked up,
    /// because [`Token::matches`] is the constant-time comparison and a map
    /// lookup would not be one.
    fn claim(self: &Arc<Self>, offered: &str) -> Option<Serving> {
        let mut live = self.lock();
        let open = live
            .iter_mut()
            .find(|open| open.token.matches(offered) && !open.serving)?;
        open.serving = true;
        Some(Serving {
            conversations: self.clone(),
            token: open.token.clone(),
            doors: open.doors.clone(),
            changed: open.changed.clone(),
            serial: self.serial.fetch_add(1, Ordering::Relaxed),
        })
    }

    fn release(&self, token: &Token) {
        if let Some(open) = self.lock().iter_mut().find(|open| &open.token == token) {
            open.serving = false;
        }
    }

    /// A poisoned lock holds a list, not a lie: the panic that poisoned it
    /// happened between two pushes, and refusing every conversation after it
    /// would end the bridge for the run.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Conversation>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One admitted stream's hold on its conversation. Dropping it frees the
/// token, which is what lets a respawned proxy dial the same session again.
struct Serving {
    conversations: Arc<Conversations>,
    token: Token,
    doors: Arc<dyn Doors>,
    changed: Arc<Notify>,
    serial: u64,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.conversations.release(&self.token);
    }
}

/// Accept until the bridge is dropped. An accept that fails is the listener
/// itself going wrong — a stream refused for its token never gets this far —
/// so the loop ends rather than spinning on it.
async fn accepting(mut listener: socket::Listener, conversations: Arc<Conversations>) {
    while let Ok(stream) = listener.accept().await {
        tokio::spawn(admitted(stream, conversations.clone()));
    }
}

/// One stream, from its first line to the end of its conversation. Every
/// refusal is a closed stream: there is nothing to say to a dialler who could
/// not say who it was.
async fn admitted(mut stream: socket::Stream, conversations: Arc<Conversations>) {
    let Ok(offered) = handshake::read(&mut stream).await else {
        return;
    };
    let Some(serving) = conversations.claim(&offered) else {
        return;
    };
    serve::serve(
        serving.doors.clone(),
        serving.changed.clone(),
        serving.serial,
        stream,
    )
    .await;
}
