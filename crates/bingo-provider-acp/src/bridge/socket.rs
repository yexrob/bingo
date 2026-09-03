//! Where the bridge listens, in the two spellings the release ships.
//!
//! One rendezvous per run (ADR-0036 §3), never a port: a unix domain socket
//! under the run's own data directory, a named pipe in the pipe namespace on
//! Windows. Both are written here, in the same change, because a platform-only
//! API without its counterpart is half a feature (AGENTS.md).
//!
//! The address is one string in both spellings — a path on unix, a pipe name
//! on Windows — because it travels through an environment variable to a
//! process that only has to dial it.

use bingo_sdk::Env;

use crate::error::AcpError;

/// The directory a run's socket lives in, under the data dir. Short on
/// purpose: `sun_path` is 104 bytes on macOS and a temporary directory on CI
/// is long before this adds anything to it.
#[cfg(unix)]
const DIRECTORY: &str = "acp";

/// Where the bridge listens: a filesystem path on unix, a named-pipe name on
/// Windows. Whatever it is, it is what the proxy reads from its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address(String);

impl Address {
    /// This run's own address. The pid makes it this process's, so a second
    /// bingo on the same data directory listens somewhere else.
    #[cfg(unix)]
    pub fn of_run(env: &Env, pid: u32) -> Self {
        Self(
            env.data_dir
                .join(DIRECTORY)
                .join(format!("{pid}.sock"))
                .display()
                .to_string(),
        )
    }

    /// The pipe namespace is flat and process-wide, so the name carries the
    /// pid where the unix path carries a directory.
    #[cfg(windows)]
    pub fn of_run(_env: &Env, pid: u32) -> Self {
        Self(format!(r"\\.\pipe\bingo-acp-{pid}"))
    }

    /// An address that already exists: the one the proxy was handed.
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ------------------------------------------------------------------- unix

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tokio::net::{UnixListener, UnixStream};

    use super::{AcpError, Address};

    /// One accepted conversation.
    pub type Stream = UnixStream;

    /// The listening end. Dropping it stops accepting and takes the socket
    /// file with it, which is how everything else in this crate ends too
    /// (`child::Adapter`, `connection::Connection`).
    pub struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    impl Listener {
        pub fn bind(address: &Address) -> Result<Self, AcpError> {
            let path = PathBuf::from(address.as_str());
            prepare(&path)?;
            let listener = UnixListener::bind(&path).map_err(|e| {
                AcpError::Spawn(format!(
                    "the tool bridge could not listen on {} ({} bytes): {e}",
                    path.display(),
                    path.as_os_str().len()
                ))
            })?;
            restrict(&path);
            Ok(Self { listener, path })
        }

        pub async fn accept(&mut self) -> Result<Stream, AcpError> {
            let (stream, _who) = self.listener.accept().await.map_err(AcpError::transport)?;
            Ok(stream)
        }
    }

    /// The directory, and no socket where this one goes. The path names this
    /// process's own pid, so anything already there is a dead run's leftover.
    fn prepare(path: &Path) -> Result<(), AcpError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AcpError::Spawn(format!("the tool bridge has no {}: {e}", parent.display()))
            })?;
        }
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    /// The token is the authority (ADR-0036 §3), and the file mode is the
    /// wall in front of it. A mode this filesystem will not take is not worth
    /// refusing the bridge over — the token still stands.
    fn restrict(path: &Path) {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

// ---------------------------------------------------------------- windows

#[cfg(windows)]
mod platform {
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    use super::{AcpError, Address};

    /// One accepted conversation.
    pub type Stream = NamedPipeServer;

    /// The listening end. A named pipe has no file to unlink: the last
    /// instance's handle closing is the whole of its removal, so dropping
    /// this is the whole of a stop.
    pub struct Listener {
        name: String,
        /// The instance the next dialler will land on. A pipe server accepts
        /// by *being* connected to, so there is always one waiting.
        idle: NamedPipeServer,
    }

    impl Listener {
        pub fn bind(address: &Address) -> Result<Self, AcpError> {
            let name = address.as_str().to_string();
            let idle = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .map_err(|e| {
                    AcpError::Spawn(format!("the tool bridge could not listen on {name}: {e}"))
                })?;
            Ok(Self { name, idle })
        }

        pub async fn accept(&mut self) -> Result<Stream, AcpError> {
            self.idle.connect().await.map_err(AcpError::transport)?;
            let waiting = ServerOptions::new()
                .create(&self.name)
                .map_err(AcpError::transport)?;
            Ok(std::mem::replace(&mut self.idle, waiting))
        }
    }
}

pub use platform::{Listener, Stream};

/// Dial the bridge. The proxy's whole knowledge of the transport.
#[cfg(unix)]
pub async fn dial(address: &Address) -> Result<Stream, AcpError> {
    tokio::net::UnixStream::connect(address.as_str())
        .await
        .map_err(|e| AcpError::transport(format!("{address}: {e}")))
}

/// The client half of a named pipe. `open` refuses while every instance is
/// busy, which is the one case a dialler may usefully wait on; the caller
/// here is a proxy the agent just spawned, and one attempt is what it has.
#[cfg(windows)]
pub async fn dial(address: &Address) -> Result<DialledStream, AcpError> {
    tokio::net::windows::named_pipe::ClientOptions::new()
        .open(address.as_str())
        .map_err(|e| AcpError::transport(format!("{address}: {e}")))
}

/// What a dial returns: the same stream on unix, the client half on Windows.
#[cfg(unix)]
pub type DialledStream = Stream;

#[cfg(windows)]
pub type DialledStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Both ends are what their two readers need of them, on both platforms.
///
/// The accepted end goes to `rmcp`; the dialled end goes to the proxy's pump,
/// which lives in the binary — and the binary's Windows cross-check is not
/// runnable without a Windows C toolchain (`aws-lc-sys`, through
/// `bingo-auth-oauth`). This crate's is, so the bound is asserted here, where
/// a named pipe that stopped satisfying it would be caught.
const _: fn() = || {
    fn pumpable<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>() {}
    pumpable::<Stream>();
    pumpable::<DialledStream>();
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The address is short by construction: a run adds one directory and a
    /// pid to the data dir, because `sun_path` is 104 bytes on macOS and CI's
    /// temporary directories are long.
    #[cfg(unix)]
    #[test]
    fn a_run_listens_under_its_own_data_directory() {
        let address = Address::of_run(&Env::rooted("/home/me"), 4321);
        assert_eq!(address.as_str(), "/home/me/.bingo/data/acp/4321.sock");
        assert!(address.as_str().len() < 104);
    }

    #[cfg(windows)]
    #[test]
    fn a_run_listens_on_a_pipe_of_its_own() {
        let address = Address::of_run(&Env::rooted("C:\\Users\\me"), 4321);
        assert_eq!(address.as_str(), r"\\.\pipe\bingo-acp-4321");
    }

    /// Two runs on one data directory do not collide.
    #[test]
    fn two_runs_do_not_share_an_address() {
        let env = Env::rooted("/home/me");
        assert_ne!(Address::of_run(&env, 1), Address::of_run(&env, 2));
    }

    #[tokio::test]
    async fn what_is_dialled_is_what_was_accepted() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let home = tempfile::tempdir().expect("a temporary home");
        let address = Address::of_run(&Env::rooted(home.path()), std::process::id());
        let mut listener = Listener::bind(&address).expect("it listens");
        let dialling = tokio::spawn({
            let address = address.clone();
            async move {
                let mut client = dial(&address).await.expect("it dials");
                client.write_all(b"hello\n").await.expect("it writes");
                client
            }
        });
        let mut accepted = listener.accept().await.expect("one arrives");
        let mut said = [0u8; 6];
        accepted.read_exact(&mut said).await.expect("it hears");
        assert_eq!(&said, b"hello\n");
        drop(dialling.await.expect("the dialler finishes"));
    }

    /// Nothing answers where nothing listens: a proxy started against a dead
    /// run fails rather than hanging.
    #[tokio::test]
    async fn dialling_an_address_nobody_listens_on_fails() {
        let home = tempfile::tempdir().expect("a temporary home");
        let address = Address::of_run(&Env::rooted(home.path()), 999_999);
        assert!(dial(&address).await.is_err());
    }

    /// A listener that is gone is gone: the file goes with it on unix, and
    /// on Windows the last instance's handle closing is the same thing.
    #[tokio::test]
    async fn a_dropped_listener_stops_answering() {
        let home = tempfile::tempdir().expect("a temporary home");
        let address = Address::of_run(&Env::rooted(home.path()), std::process::id());
        let listener = Listener::bind(&address).expect("it listens");
        drop(listener);
        assert!(dial(&address).await.is_err());
    }
}
