//! `SendFile`: the one way a file leaves this machine for a chat.
//!
//! Never a tag parsed out of the model's prose (ADR-0051 §3): a file goes out
//! because the model asked for it by name, through the gate, with the path as
//! the subject a rule can allow. The session it was asked in is what says
//! which chat — a session that is not a chat has no answer, and says so.
//!
//! The tool exists only while the channel surface runs, which is what the
//! source is for (ADR-0009 §1): a `bingo` with no chat open contributes
//! nothing rather than a tool that could only ever refuse.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSource, ToolSpec, ToolTraits, View,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::adapter::Outgoing;
use crate::directory::Directory;

/// The name the model calls it by, and the name a permission rule writes.
pub const SEND_FILE: &str = "SendFile";

/// Feishu's own ceiling on an upload, and a sane one anywhere: past this the
/// chat is the wrong way to move the file, and the model is told the size.
const MAX_BYTES: u64 = 30 * 1024 * 1024;

const DESCRIPTION: &str = "Post a file from this machine into the chat this session is \
in. Give a path, absolute or relative to the working directory; the file is uploaded and \
appears in the conversation, under the message being replied to where the chat threads. \
Use it for something a person should open or keep — a picture, a report, a log — not for \
text you could simply say. Only sessions that live in a chat can send one.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendFileArgs {
    /// Path of the file to post.
    pub path: String,
    /// A line said beside the file.
    pub caption: Option<String>,
}

/// Posting a file into the conversation the calling session lives in.
pub struct SendFile {
    directory: Directory,
}

impl SendFile {
    pub fn new(directory: Directory) -> Self {
        Self { directory }
    }

    /// The path a call names, as the gate and the reader both see it.
    fn target(input: &Value, cwd: &Path) -> Option<PathBuf> {
        let args: SendFileArgs = serde_json::from_value(input.clone()).ok()?;
        Some(resolve(&args.path, cwd))
    }
}

/// An absolute path stands; a relative one hangs off the session's working
/// directory.
fn resolve(path: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(path);
    match path.is_absolute() {
        true => path.to_path_buf(),
        false => cwd.join(path),
    }
}

/// What the file is called in the chat: its own name, never the path around it.
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
}

/// The bytes, or why they are not readable, in words the model can act on.
async fn read(path: &Path) -> Result<Vec<u8>, ToolError> {
    let shown = path.display().to_string();
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| ToolError::Failed(format!("no such file: {shown}")))?;
    if meta.is_dir() {
        return Err(ToolError::Failed(format!("is a directory: {shown}")));
    }
    if meta.len() > MAX_BYTES {
        return Err(ToolError::Failed(format!(
            "file too large to send: {} bytes, the chat takes {MAX_BYTES}",
            meta.len()
        )));
    }
    tokio::fs::read(path)
        .await
        .map_err(|e| ToolError::Failed(format!("reading {shown}: {e}")))
}

#[async_trait]
impl Tool for SendFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: SEND_FILE.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<SendFileArgs>(),
            meta: Default::default(),
        }
    }

    /// Trusted and not read-only: this plugin wrote it, and it puts a file
    /// where other people can read it. Nothing it does to the working tree,
    /// so not an edit and not destructive; one chat is one queue, so two of
    /// these must not run at once.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            trusted: true,
            ..ToolTraits::default()
        }
    }

    fn subjects(&self, input: &Value, cwd: &Path) -> Vec<Subject> {
        Self::target(input, cwd)
            .map(|path| vec![Subject::Path { path }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SendFileArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let seat = self
            .directory
            .seat(&cx.session)
            .ok_or_else(|| ToolError::Failed("this session is not a chat conversation".into()))?;
        let path = resolve(&args.path, &cx.cwd);
        let bytes = read(&path).await?;
        let files = seat
            .adapter
            .files()
            .ok_or_else(|| ToolError::Failed("this chat cannot take files".into()))?;
        let name = basename(&path);
        let sent = format!("sent {name} ({} bytes) to the chat", bytes.len());
        files
            .post(
                &seat.conversation,
                seat.parent.as_ref(),
                Outgoing {
                    name,
                    bytes,
                    caption: args.caption,
                },
            )
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut out = ToolOutput::text(sent.clone());
        out.display = Some(View::text(sent));
        Ok(out)
    }
}

/// The tool, for as long as there is a chat to send into (ADR-0009 §1).
pub struct SendFileSource {
    directory: Directory,
    tool: Arc<dyn Tool>,
}

impl SendFileSource {
    pub fn new(directory: Directory) -> Self {
        Self {
            tool: Arc::new(SendFile::new(directory.clone())) as Arc<dyn Tool>,
            directory,
        }
    }
}

#[async_trait]
impl ToolSource for SendFileSource {
    fn id(&self) -> &str {
        crate::runner::SURFACE_ID
    }

    async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        match self.directory.is_serving() {
            true => vec![Arc::clone(&self.tool)],
            false => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::ChannelAdapter;
    use crate::conversation::{Conversation, Posted};
    use crate::directory::Seat;
    use crate::loopback::{self, Loopback, Record};
    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, Env, InteractionKind, ItemBody, ItemId, KernelError,
        Prompter, SessionId, ToolHost, TurnId,
    };
    use serde_json::json;

    /// The one session every test here calls in.
    const SESSION: &str = "ses_1";

    fn seated(directory: &Directory, loopback: &Arc<Loopback>, parent: Option<Posted>) {
        directory.sit(
            SessionId::from_raw(SESSION),
            Seat {
                adapter: Arc::clone(loopback) as Arc<dyn ChannelAdapter>,
                conversation: Conversation::direct("oc_1"),
                parent,
            },
        );
    }

    fn chatting() -> (Directory, Arc<Loopback>) {
        let directory = Directory::default();
        let loopback = Arc::new(Loopback::new(loopback::Config::default()));
        seated(&directory, &loopback, None);
        (directory, loopback)
    }

    /// A call host nothing here reaches: this tool asks nobody anything.
    #[derive(Debug)]
    struct Silent;

    #[async_trait]
    impl Prompter for Silent {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            Ok(Answer::Cancel)
        }
    }

    #[async_trait]
    impl ToolHost for Silent {
        fn progress(&self, _item: &ItemId, _tail: String) {}

        async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
            Ok(ItemId::from_raw("itm_1"))
        }
    }

    /// A context whose session is the one the directory knows.
    fn context(cwd: &Path) -> ToolContext {
        ToolContext {
            call_id: "call_1".into(),
            session: SessionId::from_raw(SESSION),
            turn: TurnId::from_raw("trn_1"),
            item: ItemId::from_raw("itm_1"),
            cwd: cwd.to_path_buf(),
            cancel: CancellationToken::new(),
            env: Arc::new(Env::rooted(cwd)),
            host: bingo_sdk::testing::NoHost::handle(),
            call: Arc::new(Silent),
        }
    }

    fn wrote(dir: &Path, name: &str, body: &[u8]) {
        std::fs::write(dir.join(name), body).expect("the file is written");
    }

    #[tokio::test]
    async fn the_file_goes_to_the_chat_this_session_is_in() {
        let (directory, loopback) = chatting();
        let home = tempfile::tempdir().expect("a temporary home");
        wrote(home.path(), "notes.txt", b"by the chat\n");
        let out = SendFile::new(directory)
            .call(
                json!({ "path": "notes.txt", "caption": "the notes" }),
                &context(home.path()),
            )
            .await
            .expect("a file");
        assert_eq!(
            out.parts[0].as_text(),
            Some("sent notes.txt (12 bytes) to the chat")
        );
        assert_eq!(
            out.display,
            Some(View::text(out.parts[0].as_text().unwrap()))
        );
        assert_eq!(
            loopback.records(),
            [Record::File {
                to: Conversation::direct("oc_1"),
                parent: None,
                id: Posted::new("m1"),
                name: "notes.txt".into(),
                bytes: b"by the chat\n".to_vec(),
                caption: Some("the notes".into()),
            }]
        );
    }

    /// Under the message that is being replied to, like everything else this
    /// conversation says.
    #[tokio::test]
    async fn a_file_hangs_under_whatever_the_answer_would() {
        let directory = Directory::default();
        let loopback = Arc::new(Loopback::new(loopback::Config::default()));
        seated(&directory, &loopback, Some(Posted::new("om_parent")));
        let home = tempfile::tempdir().expect("a temporary home");
        wrote(home.path(), "notes.txt", b"x");
        SendFile::new(directory)
            .call(json!({ "path": "notes.txt" }), &context(home.path()))
            .await
            .expect("a file");
        assert!(matches!(
            &loopback.records()[0],
            Record::File { parent: Some(parent), caption: None, .. } if parent.as_str() == "om_parent"
        ));
    }

    #[tokio::test]
    async fn a_session_that_is_not_a_chat_is_refused_in_words() {
        let home = tempfile::tempdir().expect("a temporary home");
        wrote(home.path(), "notes.txt", b"x");
        let error = SendFile::new(Directory::default())
            .call(json!({ "path": "notes.txt" }), &context(home.path()))
            .await
            .expect_err("a refusal");
        assert_eq!(error.to_string(), "this session is not a chat conversation");
    }

    #[tokio::test]
    async fn a_chat_with_no_way_to_carry_a_file_says_so() {
        let directory = Directory::default();
        let loopback = Arc::new(Loopback::new(loopback::Config {
            files: false,
            ..loopback::Config::default()
        }));
        seated(&directory, &loopback, None);
        let home = tempfile::tempdir().expect("a temporary home");
        wrote(home.path(), "notes.txt", b"x");
        let error = SendFile::new(directory)
            .call(json!({ "path": "notes.txt" }), &context(home.path()))
            .await
            .expect_err("a refusal");
        assert_eq!(error.to_string(), "this chat cannot take files");
    }

    #[tokio::test]
    async fn a_path_that_does_not_read_is_named() {
        let (directory, _loopback) = chatting();
        let home = tempfile::tempdir().expect("a temporary home");
        let tool = SendFile::new(directory);
        let missing = tool
            .call(json!({ "path": "absent.txt" }), &context(home.path()))
            .await
            .expect_err("a refusal");
        assert!(
            missing.to_string().starts_with("no such file:"),
            "{missing}"
        );
        let directory_itself = tool
            .call(json!({ "path": "." }), &context(home.path()))
            .await
            .expect_err("a refusal");
        assert!(
            directory_itself.to_string().starts_with("is a directory:"),
            "{directory_itself}"
        );
        let bad_input = tool
            .call(json!({}), &context(home.path()))
            .await
            .expect_err("a refusal");
        assert!(matches!(bad_input, ToolError::InvalidInput(_)));
    }

    #[test]
    fn the_subject_is_the_resolved_path_so_a_rule_can_name_it() {
        let tool = SendFile::new(Directory::default());
        assert_eq!(
            tool.subjects(&json!({ "path": "out/report.pdf" }), Path::new("/work")),
            vec![Subject::Path {
                path: PathBuf::from("/work/out/report.pdf")
            }]
        );
        assert_eq!(
            tool.subjects(&json!({ "path": "/tmp/a.png" }), Path::new("/work")),
            vec![Subject::Path {
                path: PathBuf::from("/tmp/a.png")
            }]
        );
        let traits = tool.traits(&Value::Null);
        assert!(traits.trusted, "this plugin wrote it");
        assert!(!traits.read_only && !traits.concurrency_safe);
        assert!(!traits.edit && !traits.destructive);
    }

    #[tokio::test]
    async fn the_source_answers_nothing_until_the_surface_is_running() {
        let directory = Directory::default();
        let source = SendFileSource::new(directory.clone());
        assert!(
            source.tools().await.is_empty(),
            "no chat is open, so there is nothing to send into"
        );
        let serving = directory.serving();
        let tools = source.tools().await;
        assert_eq!(
            tools.iter().map(|t| t.spec().name).collect::<Vec<_>>(),
            [SEND_FILE]
        );
        drop(serving);
        assert!(source.tools().await.is_empty());
    }
}
