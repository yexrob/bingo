//! One spawned plugin process, and the line loop that talks to it.
//!
//! The reader owns the child's stdout and nothing else: it hands each reply to
//! whoever is waiting for that id, each `tool/progress` to whoever is running
//! that call, and each `provider/delta` to whoever is reading that stream. When
//! the pipe closes it fails every waiting request at once, so a call whose
//! process died returns instead of hanging — a bridge tool's `Interrupt` is
//! `Block`, and nothing else would ever wake it.
//!
//! A stream's queue is bounded, and the reader waits on it: a process that
//! writes faster than a turn reads blocks on its own pipe rather than growing a
//! queue in this one.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bingo_sdk::ModelEvent;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::{Sender, UnboundedSender};
use tokio::sync::oneshot;

use crate::codec::{
    Id, Message, Notification, Outcome, Request, Response, RpcError, TRANSPORT_ERROR,
};
use crate::manifest::Entry;
use crate::wire::{ProviderDeltaParams, ToolProgressParams, name};

/// What a request came back with.
pub type Reply = Result<Value, RpcError>;

/// `<data_dir>/logs/plugin-<name>.log`.
pub fn log_path(data_dir: &Path, plugin: &str) -> std::path::PathBuf {
    data_dir.join("logs").join(format!("plugin-{plugin}.log"))
}

/// Where a reply and a progress line go, and whether the process is still
/// there to send either.
#[derive(Debug, Default)]
struct Router {
    alive: AtomicBool,
    /// Set by whoever first reports the death, so it is reported once.
    announced: AtomicBool,
    waiting: Mutex<HashMap<Id, oneshot::Sender<Reply>>>,
    running: Mutex<HashMap<String, UnboundedSender<String>>>,
    streaming: Mutex<HashMap<String, Sender<ModelEvent>>>,
}

impl Router {
    fn waiting(&self) -> MutexGuard<'_, HashMap<Id, oneshot::Sender<Reply>>> {
        self.waiting.lock().unwrap_or_else(|held| held.into_inner())
    }

    fn running(&self) -> MutexGuard<'_, HashMap<String, UnboundedSender<String>>> {
        self.running.lock().unwrap_or_else(|held| held.into_inner())
    }

    fn streaming(&self) -> MutexGuard<'_, HashMap<String, Sender<ModelEvent>>> {
        self.streaming
            .lock()
            .unwrap_or_else(|held| held.into_inner())
    }

    async fn line(&self, plugin: &str, line: &str) {
        match serde_json::from_str::<Message>(line) {
            Ok(Message::Response(response)) => self.response(plugin, response),
            Ok(Message::Notification(notification)) => {
                self.notification(plugin, notification).await
            }
            Ok(Message::Request(request)) => {
                tracing::warn!(plugin, method = %request.method, "a plugin asked, which it may not");
            }
            Err(error) => {
                tracing::warn!(plugin, %error, "a plugin sent a line that is not a message");
            }
        }
    }

    fn response(&self, plugin: &str, response: Response) {
        let outcome = match response.outcome {
            Outcome::Result(value) => Ok(value),
            Outcome::Error(error) => Err(error),
        };
        let Some(id) = response.id else {
            tracing::warn!(plugin, "a plugin answered without an id");
            return;
        };
        match self.waiting().remove(&id) {
            Some(waiting) => {
                let _ = waiting.send(outcome);
            }
            None => tracing::debug!(plugin, id, "a reply nobody was waiting for"),
        }
    }

    async fn notification(&self, plugin: &str, notification: Notification) {
        match notification.method.as_str() {
            name::TOOL_PROGRESS => match serde_json::from_value(notification.params) {
                Ok(params) => self.progress(params),
                Err(error) => tracing::warn!(plugin, %error, "a progress line that is not one"),
            },
            name::PROVIDER_DELTA => match serde_json::from_value(notification.params) {
                Ok(params) => self.delta(params).await,
                Err(error) => tracing::warn!(plugin, %error, "a delta that is not an event"),
            },
            other => tracing::debug!(plugin, method = other, "an unknown notification"),
        }
    }

    fn progress(&self, params: ToolProgressParams) {
        if let Some(sink) = self.running().get(&params.call_id) {
            // A call that has already answered still has its sink until the
            // guard drops; a send nobody reads is not a problem.
            let _ = sink.send(params.tail);
        }
    }

    /// One event, to the one stream it names. The queue is bounded, so this
    /// waits when a process outruns the turn reading it — the pipe is where the
    /// backlog belongs. A stream nobody is reading any more has no sink, and
    /// its events go nowhere.
    async fn delta(&self, params: ProviderDeltaParams) {
        let sink = self.streaming().get(&params.call).cloned();
        if let Some(sink) = sink {
            let _ = sink.send(params.event).await;
        }
    }

    /// The pipe closed: nothing more will ever arrive, so everything waiting
    /// fails now rather than never.
    fn closed(&self) {
        self.alive.store(false, Ordering::Release);
        for (_, waiting) in self.waiting().drain() {
            let _ = waiting.send(Err(RpcError::new(
                TRANSPORT_ERROR,
                "the plugin process ended",
            )));
        }
        self.running().clear();
        self.streaming().clear();
    }
}

/// One live plugin process.
pub struct Connection {
    plugin: String,
    child: tokio::sync::Mutex<Child>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    next_id: AtomicI64,
    router: Arc<Router>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("plugin", &self.plugin)
            .field("alive", &self.is_alive())
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Spawn the entry, or say why not. The child's working directory is the
    /// plugin's own, so a script that reads a file beside itself finds it; the
    /// directory a call is about travels in the call, never in the process.
    pub fn spawn(
        plugin: &str,
        entry: &Entry,
        root: &Path,
        data_dir: &Path,
    ) -> Result<Self, String> {
        let mut command = Command::new(&entry.command);
        command
            .args(&entry.args)
            .envs(&entry.env)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_sink(data_dir, plugin))
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| format!("spawning {}: {e}", entry.command))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("the child was spawned without pipes".to_string());
        };
        Ok(Self::over(plugin, child, stdin, stdout))
    }

    fn over(plugin: &str, child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let router = Arc::new(Router {
            alive: AtomicBool::new(true),
            ..Router::default()
        });
        tokio::spawn(pump(plugin.to_string(), stdout, Arc::clone(&router)));
        Self {
            plugin: plugin.to_string(),
            child: tokio::sync::Mutex::new(child),
            stdin: tokio::sync::Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            router,
        }
    }

    /// Whether the process is still on the other end of the pipe.
    pub fn is_alive(&self) -> bool {
        self.router.alive.load(Ordering::Acquire)
    }

    /// True for the first caller to find this process gone, so one death is
    /// one notice however many readers noticed it.
    pub fn claim_death(&self) -> bool {
        !self.is_alive() && !self.router.announced.swap(true, Ordering::AcqRel)
    }

    /// Ask, and wait for the answer or for the process to end.
    pub async fn request(&self, method: &str, params: Value) -> Reply {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, answer) = oneshot::channel();
        self.router.waiting().insert(id, sender);
        if let Err(why) = self
            .write(Message::Request(Request::new(id, method, params)))
            .await
        {
            self.router.waiting().remove(&id);
            return Err(RpcError::new(TRANSPORT_ERROR, why));
        }
        match answer.await {
            Ok(reply) => reply,
            // The router drops a sender only when the pipe closed.
            Err(_) => Err(RpcError::new(TRANSPORT_ERROR, "the plugin process ended")),
        }
    }

    /// Tell, and do not wait. A notification the process never reads is not
    /// an error the caller can do anything about.
    pub async fn notify(&self, method: &str, params: Value) {
        if let Err(why) = self
            .write(Message::Notification(Notification::new(method, params)))
            .await
        {
            tracing::debug!(plugin = %self.plugin, method, %why, "a notification went nowhere");
        }
    }

    /// Route this call's `tool/progress` lines to `sink` until the guard drops.
    pub fn watch(self: &Arc<Self>, call_id: &str, sink: UnboundedSender<String>) -> Watch {
        self.router.running().insert(call_id.to_string(), sink);
        Watch {
            router: Arc::clone(&self.router),
            call_id: call_id.to_string(),
        }
    }

    /// Route this stream's `provider/delta` events to `sink` until the guard
    /// drops. Two streams on one pipe are two routes, told apart by `call`.
    pub fn watch_stream(self: &Arc<Self>, call: &str, sink: Sender<ModelEvent>) -> StreamWatch {
        self.router.streaming().insert(call.to_string(), sink);
        StreamWatch {
            router: Arc::clone(&self.router),
            call: call.to_string(),
        }
    }

    /// A name for one stream on this pipe, unique while the process lives. The
    /// counter is the request ids', so one process mints one series.
    pub fn next_call(&self) -> String {
        format!("call-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// End the process. The host is closing, or the plugin refused the
    /// handshake and will never be spoken to again.
    pub async fn stop(&self) {
        self.router.closed();
        if let Err(error) = self.child.lock().await.kill().await {
            tracing::debug!(plugin = %self.plugin, %error, "the plugin process was already gone");
        }
    }

    async fn write(&self, message: Message) -> Result<(), String> {
        let line = message.line().map_err(|e| e.to_string())?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }
}

/// Removes a call's progress route when the call is over.
pub struct Watch {
    router: Arc<Router>,
    call_id: String,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.router.running().remove(&self.call_id);
    }
}

/// Removes a stream's delta route when nobody is reading it any more.
pub struct StreamWatch {
    router: Arc<Router>,
    call: String,
}

impl Drop for StreamWatch {
    fn drop(&mut self) {
        self.router.streaming().remove(&self.call);
    }
}

/// The reader: every line the process writes, until it writes no more.
async fn pump(plugin: String, stdout: ChildStdout, router: Arc<Router>) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => router.line(&plugin, &line).await,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(plugin, %error, "the plugin's output could not be read");
                break;
            }
        }
    }
    router.closed();
}

/// Where a child's stderr goes. Left alone it inherits the terminal and paints
/// over a full-screen TUI, which never redraws its scrollback; a log file that
/// will not open sends it to the void rather than to the screen.
fn stderr_sink(data_dir: &Path, plugin: &str) -> Stdio {
    match open_log(data_dir, plugin) {
        Ok(file) => Stdio::from(file),
        Err(error) => {
            tracing::warn!(
                plugin,
                %error,
                "no log for this plugin's stderr; discarding it"
            );
            Stdio::null()
        }
    }
}

fn open_log(data_dir: &Path, plugin: &str) -> std::io::Result<std::fs::File> {
    let path = log_path(data_dir, plugin);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use serde_json::json;

    fn entry(command: &str, args: &[&str]) -> Entry {
        Entry {
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: Default::default(),
        }
    }

    #[test]
    fn a_plugin_logs_its_stderr_under_the_data_directory() {
        assert_eq!(
            log_path(Path::new("/home/u/.bingo/data"), "wordcount"),
            PathBuf::from("/home/u/.bingo/data/logs/plugin-wordcount.log")
        );
    }

    #[test]
    fn a_command_that_does_not_exist_says_so_rather_than_pretending() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let error = Connection::spawn(
            "missing",
            &entry("bingo-no-such-plugin", &[]),
            dir.path(),
            dir.path(),
        )
        .expect_err("there is no such command");
        assert!(
            error.starts_with("spawning bingo-no-such-plugin"),
            "{error}"
        );
    }

    #[test]
    fn a_log_the_process_cannot_open_sends_stderr_to_the_void_not_the_screen() {
        // A data directory that is a file has no `logs` directory to create.
        let file = tempfile::NamedTempFile::new().expect("a temporary file");
        drop(stderr_sink(file.path(), "wordcount"));
    }

    /// `cat` echoes nothing back, so the request is still waiting when the
    /// process ends: the reader's `closed` is what wakes it.
    #[tokio::test]
    async fn a_request_whose_process_ends_fails_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let connection = Connection::spawn("quiet", &entry("true", &[]), dir.path(), dir.path())
            .expect("`true` exists on every unix");
        let error = connection
            .request("initialize", json!({}))
            .await
            .expect_err("a process that says nothing answers nothing");
        assert_eq!(error.code, TRANSPORT_ERROR);
        assert!(!connection.is_alive());
        assert!(connection.claim_death(), "the first to notice reports it");
        assert!(!connection.claim_death(), "and only the first");
    }

    #[tokio::test]
    async fn a_line_that_is_not_a_message_is_ignored_not_fatal() {
        let router = Router {
            alive: AtomicBool::new(true),
            ..Router::default()
        };
        router.line("noisy", "this is not json").await;
        router
            .line("noisy", r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
            .await;
        assert!(router.alive.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn a_progress_line_reaches_the_call_it_names_and_no_other() {
        let router = Router {
            alive: AtomicBool::new(true),
            ..Router::default()
        };
        let (sender, mut tail) = tokio::sync::mpsc::unbounded_channel();
        router.running().insert("call_1".to_string(), sender);
        router.line(
            "noisy",
            r#"{"jsonrpc":"2.0","method":"tool/progress","params":{"callId":"call_1","tail":"half way"}}"#,
        ).await;
        router.line(
            "noisy",
            r#"{"jsonrpc":"2.0","method":"tool/progress","params":{"callId":"call_2","tail":"nobody"}}"#,
        ).await;
        assert_eq!(tail.recv().await.as_deref(), Some("half way"));
        assert!(
            tail.try_recv().is_err(),
            "the other call's line went nowhere"
        );
    }

    fn delta(call: &str, text: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","method":"provider/delta","params":{{"call":"{call}","event":{{"type":"textDelta","id":"b1","delta":"{text}"}}}}}}"#
        )
    }

    /// The defect the `call` key exists to make impossible: two streams on one
    /// pipe, and neither sees the other's events.
    #[tokio::test]
    async fn a_delta_reaches_the_stream_it_names_and_no_other() {
        let router = Router {
            alive: AtomicBool::new(true),
            ..Router::default()
        };
        let (one, mut first) = tokio::sync::mpsc::channel(4);
        let (two, mut second) = tokio::sync::mpsc::channel(4);
        router.streaming().insert("call-1".to_string(), one);
        router.streaming().insert("call-2".to_string(), two);
        router.line("noisy", &delta("call-1", "mine")).await;
        router.line("noisy", &delta("call-2", "yours")).await;
        router.line("noisy", &delta("call-3", "nobody's")).await;
        assert_eq!(
            first.recv().await,
            Some(ModelEvent::TextDelta {
                id: "b1".into(),
                delta: "mine".into()
            })
        );
        assert_eq!(
            second.recv().await,
            Some(ModelEvent::TextDelta {
                id: "b1".into(),
                delta: "yours".into()
            })
        );
        assert!(first.try_recv().is_err() && second.try_recv().is_err());
    }

    /// The pipe closed: a stream waiting on it is woken by the sink going,
    /// exactly as a waiting request is woken by its error.
    #[tokio::test]
    async fn a_process_that_ends_ends_every_stream_it_was_feeding() {
        let router = Router {
            alive: AtomicBool::new(true),
            ..Router::default()
        };
        let (sender, mut events) = tokio::sync::mpsc::channel(4);
        router.streaming().insert("call-1".to_string(), sender);
        router.closed();
        assert_eq!(events.recv().await, None);
    }

    #[tokio::test]
    async fn one_process_mints_one_series_of_stream_names() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let connection = Connection::spawn("quiet", &entry("cat", &[]), dir.path(), dir.path())
            .expect("`cat` exists on every unix");
        assert_ne!(connection.next_call(), connection.next_call());
    }
}
