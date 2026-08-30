//! What the turn leaves behind: a working turn is asked, once, for the facts
//! worth keeping.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, Hook, HookContext, HookMatcher, HookPoint, Item, ItemBody, ItemStatus,
    Message, ModelRequest, Phase, ProviderMetadata, Role, SystemBlock, TurnId,
};

use crate::{memory, root, stream, tail, transcript};

const EXTRACT: &str = "\
You are a memory extractor. Extract project facts worth remembering long-term from the agent \
conversation below:
- Project structure conventions and key file paths
- Architecture decisions and their rationale
- Build/test commands and conventions
- User preferences and constraints
Output only a fact list, one per line: no numbering, no pleasantries. Output nothing when no \
fact is worth remembering.";

/// What the extractor may say. A fact is a line, and a page of them is more
/// than one turn learned.
const MAX_TOKENS: u32 = 1_024;

/// How much of the turn it reads, newest kept.
const MAX_CHARS: u64 = 60_000;

/// The turn is not held open for a memory. Everything here runs before the
/// turn's outcome is returned, so the model gets one bounded chance.
const DEADLINE: Duration = Duration::from_secs(30);

/// Asks the model, at the end of a turn that used a tool, what this project
/// taught it.
#[derive(Debug, Clone)]
pub struct MemoryHook {
    data_dir: PathBuf,
}

impl MemoryHook {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    async fn remember(&self, items: &[Item], cx: &HookContext) {
        let Some(facts) = extract(items, cx).await else {
            return;
        };
        let root = root::of(&cx.cwd).await;
        let path = memory::path(&self.data_dir, &root);
        if let Err(error) = append(&path, &facts).await {
            tracing::warn!(%error, path = %path.display(), "memory: nothing was written");
        }
    }
}

#[async_trait]
impl Hook for MemoryHook {
    fn id(&self) -> &str {
        "context:memory"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Turn],
            tool: None,
        }
    }

    async fn on_turn(&self, phase: Phase, _turn: &TurnId, items: &[Item], cx: &HookContext) {
        if phase == Phase::End {
            self.remember(items, cx).await;
        }
    }
}

/// The facts the model found, or nothing at all: a turn that ran no tool did
/// no work worth remembering, and a session with no provider cannot ask.
async fn extract(items: &[Item], cx: &HookContext) -> Option<String> {
    if !items.iter().any(worked) {
        return None;
    }
    let (provider, model) = (cx.provider.as_ref()?, cx.model.as_ref()?);
    let request = request(model, items);
    let asked = stream::drain(provider.as_ref(), request, CancellationToken::new());
    match tokio::time::timeout(DEADLINE, asked).await {
        Ok(Ok(answer)) => Some(answer.text).filter(|text| !text.trim().is_empty()),
        Ok(Err(error)) => {
            tracing::warn!(%error, "memory: the extractor did not answer");
            None
        }
        Err(_) => {
            tracing::warn!("memory: the extractor ran out of time");
            None
        }
    }
}

fn worked(item: &Item) -> bool {
    item.status == ItemStatus::Completed && matches!(item.body, ItemBody::ToolCall { .. })
}

fn request(model: &str, items: &[Item]) -> ModelRequest {
    ModelRequest {
        model: model.to_string(),
        max_tokens: MAX_TOKENS,
        system: vec![SystemBlock {
            text: EXTRACT.to_string(),
            cache: false,
        }],
        messages: vec![Message::text(Role::User, body(items))],
        tools: Vec::new(),
        reasoning: None,
        provider_options: ProviderMetadata::new(),
    }
}

/// The turn as the extractor reads it: a long turn's whole transcript would
/// cost more than the facts in it are worth.
fn body(items: &[Item]) -> String {
    let lines = transcript::lines(items);
    let dropped = tail::first_within(&lines, MAX_CHARS, |l| l.chars().count() as u64 + 1);
    lines[dropped..].join("\n")
}

/// The whole file, replaced in one rename: a memory half-written by a turn
/// that was interrupted is worse than a memory one turn out of date.
async fn append(path: &Path, facts: &str) -> std::io::Result<()> {
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let Some(next) = memory::merged(&existing, facts) else {
        return Ok(());
    };
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    tokio::fs::create_dir_all(dir).await?;
    let tmp = path.with_extension("md.tmp");
    tokio::fs::write(&tmp, next).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{tool, user};
    use crate::scripted::Scripted;
    use bingo_sdk::{Provider, ProviderError, SessionId};
    use std::sync::Arc;

    struct Session {
        data: tempfile::TempDir,
        cwd: tempfile::TempDir,
    }

    impl Session {
        fn new() -> Self {
            Self {
                data: tempfile::tempdir().expect("a data dir"),
                cwd: tempfile::tempdir().expect("a cwd"),
            }
        }

        fn hook(&self) -> MemoryHook {
            MemoryHook::new(self.data.path().to_path_buf())
        }

        fn context(&self, provider: Option<Arc<dyn Provider>>) -> HookContext {
            HookContext {
                host: bingo_sdk::testing::NoHost::handle(),
                session: SessionId::from_raw("ses_test"),
                turn: Some(TurnId::from_raw("trn_1")),
                cwd: self.cwd.path().to_path_buf(),
                provider,
                model: provider_model(),
            }
        }

        fn path(&self) -> PathBuf {
            let root = self.cwd.path().canonicalize().unwrap_or_default();
            memory::path(self.data.path(), &root)
        }

        fn memory(&self) -> String {
            std::fs::read_to_string(self.path()).unwrap_or_default()
        }

        fn write(&self, text: &str) {
            let path = self.path();
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the memory dir");
            std::fs::write(path, text).expect("the memory");
        }
    }

    fn provider_model() -> Option<String> {
        Some("model-x".to_string())
    }

    fn working_turn() -> Vec<Item> {
        vec![
            user("u", "run the tests"),
            tool("t", "Bash", r#"{"command":"cargo test"}"#, Some("ok")),
        ]
    }

    async fn end(session: &Session, items: &[Item], provider: Option<Arc<dyn Provider>>) {
        session
            .hook()
            .on_turn(
                Phase::End,
                &TurnId::from_raw("trn_1"),
                items,
                &session.context(provider),
            )
            .await;
    }

    #[test]
    fn it_listens_at_the_turn_only() {
        let hook = MemoryHook::new(PathBuf::from("/data"));
        assert_eq!(hook.id(), "context:memory");
        assert_eq!(hook.matcher().points, [HookPoint::Turn]);
        assert!(hook.matcher().tool.is_none());
    }

    #[tokio::test]
    async fn a_working_turn_leaves_its_facts_behind() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("the tests run with cargo test\n"));
        end(&session, &working_turn(), Some(provider.clone())).await;
        assert_eq!(session.memory(), "the tests run with cargo test\n");
        let request = provider.requests().remove(0);
        assert!(
            request.system[0]
                .text
                .starts_with("You are a memory extractor")
        );
        assert_eq!(request.max_tokens, MAX_TOKENS);
    }

    #[tokio::test]
    async fn a_fact_the_file_already_holds_is_not_written_twice() {
        let session = Session::new();
        session.write("the tests run with cargo test\n");
        let provider = Arc::new(Scripted::saying("the tests run with cargo test"));
        end(&session, &working_turn(), Some(provider)).await;
        assert_eq!(session.memory(), "the tests run with cargo test\n");
    }

    #[tokio::test]
    async fn the_oldest_facts_are_evicted_past_the_cap() {
        let session = Session::new();
        session.write(&(1..=300).map(|i| format!("fact {i}\n")).collect::<String>());
        let provider = Arc::new(Scripted::saying("fact 301"));
        end(&session, &working_turn(), Some(provider)).await;
        let memory = session.memory();
        assert_eq!(memory.lines().count(), memory::MAX_LINES);
        assert_eq!(memory.lines().next(), Some("fact 2"));
        assert_eq!(memory.lines().last(), Some("fact 301"));
    }

    #[tokio::test]
    async fn a_turn_that_ran_no_tool_is_never_asked() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("a fact"));
        end(&session, &[user("u", "hello")], Some(provider.clone())).await;
        assert!(provider.requests().is_empty());
        assert!(session.memory().is_empty());
    }

    #[tokio::test]
    async fn a_session_without_a_provider_writes_nothing() {
        let session = Session::new();
        end(&session, &working_turn(), None).await;
        assert!(session.memory().is_empty());
    }

    #[tokio::test]
    async fn a_provider_that_refuses_leaves_the_file_alone() {
        let session = Session::new();
        session.write("the tests run with cargo test\n");
        let provider = Arc::new(Scripted::failing(ProviderError::Auth {
            message: "no key".into(),
        }));
        end(&session, &working_turn(), Some(provider)).await;
        assert_eq!(session.memory(), "the tests run with cargo test\n");
    }

    #[tokio::test]
    async fn an_empty_answer_writes_nothing() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("  \n "));
        end(&session, &working_turn(), Some(provider)).await;
        assert!(session.memory().is_empty());
    }

    #[tokio::test]
    async fn the_start_of_a_turn_does_nothing() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("a fact"));
        session
            .hook()
            .on_turn(
                Phase::Start,
                &TurnId::from_raw("trn_1"),
                &working_turn(),
                &session.context(Some(provider.clone())),
            )
            .await;
        assert!(provider.requests().is_empty());
    }

    #[test]
    fn a_long_turn_is_read_from_its_newest_end() {
        let items: Vec<Item> = (0..500)
            .map(|i| user(&format!("u{i}"), &"x".repeat(400)))
            .collect();
        let body = body(&items);
        assert!(body.chars().count() as u64 <= MAX_CHARS);
        assert!(body.ends_with(&"x".repeat(400)));
    }
}
