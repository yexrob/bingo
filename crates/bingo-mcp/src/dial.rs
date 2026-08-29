//! One server's handshake: spawn or connect, initialize, list the tools.
//!
//! Nothing of the manager is borrowed here, which is what lets a list of
//! servers be dialled at once rather than one after another; filing the
//! outcome is the manager's job, under a lock this module never holds.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::{RoleClient, serve_client};
use tokio::process::Command;

use crate::config::Server;

/// How long one server has to spawn, initialize and answer `tools/list`.
///
/// A server that hangs occupies its own task for this long and nothing else:
/// the first turn may start before it lands, and a server that misses the
/// deadline is a failure a person retries with `/mcp reconnect`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The client side of one live server.
pub type Service = RunningService<RoleClient, ()>;

/// What a handshake leaves behind: the running service, and what it said it can do.
pub struct Connection {
    pub service: Arc<Service>,
    pub tools: Vec<rmcp::model::Tool>,
}

/// `<data_dir>/logs/mcp-<server>.log`.
pub fn log_path(data_dir: &Path, server: &str) -> PathBuf {
    data_dir.join("logs").join(format!("mcp-{server}.log"))
}

/// Dial one server, or say why not. Never panics and never waits longer than
/// [`CONNECT_TIMEOUT`].
pub async fn dial(
    server_name: &str,
    server: &Server,
    data_dir: &Path,
) -> Result<Connection, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, connect(server_name, server, data_dir)).await {
        Ok(outcome) => outcome,
        Err(_) => Err(format!(
            "connect timed out after {}s",
            CONNECT_TIMEOUT.as_secs()
        )),
    }
}

async fn connect(
    server_name: &str,
    server: &Server,
    data_dir: &Path,
) -> Result<Connection, String> {
    match server {
        Server::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let service =
                spawn_child(server_name, command, args, env, cwd.as_deref(), data_dir).await?;
            list_tools(service).await
        }
        // Every failure of an HTTP dial goes through one place, because every
        // one of them may print the URI that was dialled.
        Server::Http { url, headers } => over_http(url, headers)
            .await
            .map_err(|why| redact(&why, url)),
    }
}

async fn over_http(url: &str, headers: &BTreeMap<String, String>) -> Result<Connection, String> {
    list_tools(open_http(url, headers).await?).await
}

/// What the server says it can do, asked once, at the end of the handshake.
async fn list_tools(service: Service) -> Result<Connection, String> {
    let tools = service
        .list_all_tools()
        .await
        .map_err(|e| format!("listing tools: {e}"))?;
    Ok(Connection {
        service: Arc::new(service),
        tools,
    })
}

async fn spawn_child(
    server_name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&Path>,
    data_dir: &Path,
) -> Result<Service, String> {
    let mut child = Command::new(command);
    child.args(args).envs(env);
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    let (transport, _stderr) = TokioChildProcess::builder(child)
        .stderr(stderr_sink(data_dir, server_name))
        .spawn()
        .map_err(|e| format!("spawning {command}: {e}"))?;
    serve_client((), transport)
        .await
        .map_err(|e| format!("handshake: {e}"))
}

async fn open_http(url: &str, headers: &BTreeMap<String, String>) -> Result<Service, String> {
    let config =
        StreamableHttpClientTransportConfig::with_uri(url).custom_headers(http_headers(headers)?);
    serve_client((), StreamableHttpClientTransport::from_config(config))
        .await
        .map_err(|e| format!("handshake: {e}"))
}

/// A header value is where a person keeps their token, so a value this crate
/// cannot use is reported by the name it was given under and nothing else.
fn http_headers(
    headers: &BTreeMap<String, String>,
) -> Result<HashMap<HeaderName, HeaderValue>, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let header = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("http header {name}: {e}"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| format!("http header {name}: the value is not a header value"))?;
            Ok((header, value))
        })
        .collect()
}

/// A transport error prints the URI it dialled, and a configured URI may carry
/// a credential in its query; what a person reads keeps the endpoint and drops
/// the rest.
fn redact(message: &str, url: &str) -> String {
    match url.split_once('?') {
        Some((endpoint, _)) => message.replace(url, endpoint),
        None => message.to_string(),
    }
}

/// Where a child's stderr goes. Left alone it inherits the terminal and paints
/// over a full-screen TUI, which never redraws its scrollback; a log file that
/// will not open sends it to the void rather than to the screen.
fn stderr_sink(data_dir: &Path, server_name: &str) -> Stdio {
    match open_log(data_dir, server_name) {
        Ok(file) => Stdio::from(file),
        Err(error) => {
            tracing::warn!(
                server = server_name,
                %error,
                "no log for this mcp server's stderr; discarding it"
            );
            Stdio::null()
        }
    }
}

fn open_log(data_dir: &Path, server_name: &str) -> std::io::Result<std::fs::File> {
    let path = log_path(data_dir, server_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_logs_its_stderr_under_the_data_directory() {
        assert_eq!(
            log_path(Path::new("/home/u/.bingo/data"), "files"),
            PathBuf::from("/home/u/.bingo/data/logs/mcp-files.log")
        );
    }

    #[test]
    fn a_header_this_crate_cannot_use_is_named_without_its_value() {
        let headers = BTreeMap::from([("Authorization".to_string(), "Bearer \n bad".to_string())]);
        let error = http_headers(&headers).expect_err("refused");
        assert!(error.contains("Authorization"), "{error}");
        assert!(!error.contains("Bearer"), "{error}");
    }

    #[test]
    fn a_readable_header_reaches_the_transport() {
        let headers = BTreeMap::from([("Authorization".to_string(), "Bearer token".to_string())]);
        let built = http_headers(&headers).expect("a usable header");
        assert_eq!(
            built.get(&HeaderName::from_static("authorization")),
            Some(&HeaderValue::from_static("Bearer token"))
        );
    }

    #[test]
    fn a_url_in_a_failure_keeps_its_endpoint_and_drops_its_query() {
        let url = "https://mcp.example.com/mcp?key=s3cret";
        let message = redact(
            &format!("handshake: error sending request for url ({url})"),
            url,
        );
        assert_eq!(
            message,
            "handshake: error sending request for url (https://mcp.example.com/mcp)"
        );
    }

    #[test]
    fn a_url_without_a_query_is_left_as_it_is() {
        let url = "https://mcp.example.com/mcp";
        assert_eq!(redact("handshake: refused", url), "handshake: refused");
    }

    #[test]
    fn a_log_the_process_cannot_open_sends_stderr_to_the_void_not_the_screen() {
        // A data directory that is a file has no `logs` directory to create.
        let file = tempfile::NamedTempFile::new().expect("a temporary file");
        let sink = stderr_sink(file.path(), "files");
        // `Stdio` says nothing about itself; that this returned at all is the
        // point — the fallback never propagates and never inherits.
        drop(sink);
    }

    /// Nothing is listening on port 1, so this is the failure path an HTTP
    /// server takes; whatever the transport says about it, the credential a
    /// person put in the query is not in it.
    #[tokio::test]
    async fn a_failed_http_dial_never_reports_a_credential_from_the_url() {
        let server = Server::Http {
            url: "http://127.0.0.1:1/mcp?key=s3cret".into(),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer t0ken".to_string())]),
        };
        let dir = tempfile::tempdir().expect("a temporary directory");
        let Err(why) = dial("remote", &server, dir.path()).await else {
            panic!("nothing is listening on port 1");
        };
        assert!(!why.contains("s3cret"), "{why}");
        assert!(!why.contains("t0ken"), "{why}");
    }

    #[tokio::test]
    async fn a_command_that_does_not_exist_fails_without_waiting() {
        let server = Server::Stdio {
            command: "bingo-no-such-mcp-server".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        };
        let dir = tempfile::tempdir().expect("a temporary directory");
        let Err(error) = dial("missing", &server, dir.path()).await else {
            panic!("there is no such command to dial");
        };
        assert!(
            error.starts_with("spawning bingo-no-such-mcp-server"),
            "{error}"
        );
    }
}
