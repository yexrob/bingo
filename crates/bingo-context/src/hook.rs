//! What the turn leaves behind: a working turn is asked, once, for the facts
//! worth keeping.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, Hook, HookContext, HookMatcher, HookPoint, Item, ItemBody, ItemStatus,
    Message, ModelRequest, Phase, ProviderMetadata, Role, SystemBlock, TurnId,
};

use crate::memory::file::{self, Kind, Memory};
use crate::memory::{dir, migrate, store};
use crate::{root, stream, tail, transcript};

const EXTRACT: &str = "\
You are a memory extractor. From the agent conversation below, extract only what is worth \
remembering across sessions and is not already recorded in the repository (its files, \
instructions, commit history):
- user: who the person is, how they work, what they prefer
- feedback: a correction they made or an approach they confirmed, and why
- project: a goal, decision or constraint the repository does not record (dates absolute)
- reference: a URL, ticket or dashboard worth finding again
Output one fact per line as `<type>: <fact>`, no numbering, no pleasantries. Leave out what \
matters only to this conversation. Never output a secret, a key or a token, and name people \
by their role, not their name. Output nothing when no fact is worth remembering.";

/// What the extractor may say. A fact is a line, and a page of them is more
/// than one turn learned.
const MAX_TOKENS: u32 = 1_024;

/// How much of the turn it reads, newest kept.
const MAX_CHARS: u64 = 60_000;

/// The turn is not held open for a memory. Everything here runs before the
/// turn's outcome is returned, so the model gets one bounded chance.
const DEADLINE: Duration = Duration::from_secs(30);

/// What a fact's description may spend. A fact the extractor found is one
/// line, so the line is its own description; a long one is cut, and the whole
/// of it stays in the file.
const DESCRIPTION_CHARS: usize = 120;

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
        migrate::once(&self.data_dir, &root).await;
        for line in facts.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (kind, fact) = typed(line);
            // What is true of the person is true in every project.
            let scope = match kind {
                Kind::User => dir::user(&self.data_dir),
                _ => dir::project(&self.data_dir, &root),
            };
            if let Err(error) = keep(&scope, kind, fact).await {
                tracing::warn!(%error, path = %scope.display(), "memory: a fact was not written");
            }
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
        // A side question, not the session's turn: it belongs to no
        // conversation a stateful provider is keeping.
        session: None,
        provider_options: side_question("memory"),
    }
}

/// A request that is a question about the conversation, not the
/// conversation: named as such, so a provider that answers from a script
/// can tell the two apart. A real provider ignores it.
fn side_question(purpose: &str) -> ProviderMetadata {
    let mut about = serde_json::Map::new();
    about.insert("purpose".into(), serde_json::Value::String(purpose.into()));
    ProviderMetadata::from([("bingo".to_string(), about)])
}

/// The turn as the extractor reads it: a long turn's whole transcript would
/// cost more than the facts in it are worth.
fn body(items: &[Item]) -> String {
    let lines = transcript::lines(items);
    let dropped = tail::first_within(&lines, MAX_CHARS, |l| l.chars().count() as u64 + 1);
    lines[dropped..].join("\n")
}

/// One fact as one file, named after its first words. A name the scope
/// already holds is left alone: whoever wrote that file knew more than a
/// line, and a lost extraction is the cheap side of the collision.
async fn keep(scope: &Path, kind: Kind, fact: &str) -> std::io::Result<()> {
    let Some(name) = file::slug(fact) else {
        return Ok(());
    };
    if store::holds(scope, &name).await {
        return Ok(());
    }
    store::save(scope, &remembered(name, kind, fact)).await
}

/// The type the extractor named and the fact after it; a line with no type
/// it was asked for is a project fact, which is what an untyped line was.
fn typed(line: &str) -> (Kind, &str) {
    line.split_once(':')
        .and_then(|(word, rest)| Some((Kind::of(word.trim())?, rest.trim())))
        .filter(|(_, fact)| !fact.is_empty())
        .unwrap_or((Kind::Project, line))
}

fn remembered(name: String, kind: Kind, fact: &str) -> Memory {
    Memory {
        name,
        description: cut(fact, DESCRIPTION_CHARS),
        kind,
        body: format!("{fact}\n"),
    }
}

/// The first `chars` characters, and an ellipsis when there were more.
fn cut(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    text.chars().take(chars).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{tool, user};
    use crate::memory::index;
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

        fn scope(&self) -> PathBuf {
            let root = self.cwd.path().canonicalize().unwrap_or_default();
            dir::project(self.data.path(), &root)
        }

        async fn memories(&self) -> Vec<Memory> {
            store::list(&self.scope()).await
        }

        async fn user_memories(&self) -> Vec<Memory> {
            store::list(&dir::user(self.data.path())).await
        }

        async fn index(&self) -> String {
            store::index_text(&self.scope()).await
        }

        async fn write(&self, memory: &Memory) {
            store::save(&self.scope(), memory).await.expect("a memory");
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
    async fn a_working_turn_leaves_one_file_per_fact_and_one_line_each() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying(
            "the tests run with cargo test\nthe kernel imports no plugin\n",
        ));
        end(&session, &working_turn(), Some(provider.clone())).await;

        let memories = session.memories().await;
        assert_eq!(
            memories.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            [
                "the-kernel-imports-no-plugin",
                "the-tests-run-with-cargo-test"
            ]
        );
        assert!(memories.iter().all(|m| m.kind == Kind::Project));
        assert_eq!(memories[1].description, "the tests run with cargo test");
        assert_eq!(memories[1].body, "the tests run with cargo test\n");
        assert_eq!(
            session
                .index()
                .await
                .lines()
                .filter_map(index::parse)
                .map(|entry| entry.slug)
                .collect::<Vec<_>>(),
            [
                "the-tests-run-with-cargo-test",
                "the-kernel-imports-no-plugin"
            ],
        );

        let request = provider.requests().remove(0);
        assert!(
            request.system[0]
                .text
                .starts_with("You are a memory extractor")
        );
        assert_eq!(request.max_tokens, MAX_TOKENS);
    }

    /// The extractor names the type of each fact, and a fact about the
    /// person goes where every project reads it. A line with no type it was
    /// asked for is a project fact, as every line was before.
    #[tokio::test]
    async fn a_typed_fact_goes_to_its_scope_and_an_untyped_one_is_the_projects() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying(
            "user: prefers short answers\nfeedback: never push without a word\nbare fact\n",
        ));
        end(&session, &working_turn(), Some(provider)).await;
        let project = session.memories().await;
        assert_eq!(
            project
                .iter()
                .map(|m| (m.name.as_str(), m.kind))
                .collect::<Vec<_>>(),
            [
                ("bare-fact", Kind::Project),
                ("never-push-without-a-word", Kind::Feedback),
            ]
        );
        let user = session.user_memories().await;
        assert_eq!(
            user.iter()
                .map(|m| (m.name.as_str(), m.kind, m.body.as_str()))
                .collect::<Vec<_>>(),
            [(
                "prefers-short-answers",
                Kind::User,
                "prefers short answers\n"
            )]
        );
    }

    #[tokio::test]
    async fn a_name_the_scope_already_holds_is_left_exactly_as_it_was() {
        let session = Session::new();
        let written = Memory {
            name: "the-tests-run-with-cargo-test".into(),
            description: "how this project is tested".into(),
            kind: Kind::Project,
            body: "run it from the workspace root, always with --locked\n".into(),
        };
        session.write(&written).await;
        let provider = Arc::new(Scripted::saying("the tests run with cargo test"));
        end(&session, &working_turn(), Some(provider)).await;
        assert_eq!(session.memories().await, [written]);
        assert_eq!(session.index().await.lines().count(), 1);
    }

    #[tokio::test]
    async fn a_fact_no_file_name_can_be_made_of_is_dropped() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("!!! ???\n***"));
        end(&session, &working_turn(), Some(provider)).await;
        assert!(session.memories().await.is_empty());
    }

    #[tokio::test]
    async fn a_long_fact_keeps_its_whole_self_in_the_file() {
        let session = Session::new();
        let fact = "the build ".repeat(40);
        let provider = Arc::new(Scripted::saying(&fact));
        end(&session, &working_turn(), Some(provider)).await;
        let memories = session.memories().await;
        assert_eq!(memories[0].body, format!("{}\n", fact.trim()));
        assert_eq!(
            memories[0].description.chars().count(),
            DESCRIPTION_CHARS + 1
        );
        assert!(memories[0].description.ends_with('…'));
    }

    #[tokio::test]
    async fn the_old_single_file_is_migrated_before_a_fact_is_added() {
        let session = Session::new();
        let root = session.cwd.path().canonicalize().expect("a real path");
        let old = dir::legacy(session.data.path(), &root);
        std::fs::create_dir_all(old.parent().expect("a parent")).expect("the memory dir");
        std::fs::write(&old, "an older fact\n").expect("the old file");

        let provider = Arc::new(Scripted::saying("the tests run with cargo test"));
        end(&session, &working_turn(), Some(provider)).await;
        let names: Vec<String> = session
            .memories()
            .await
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(names, ["imported", "the-tests-run-with-cargo-test"]);
        assert!(!old.exists());
    }

    #[tokio::test]
    async fn a_turn_that_ran_no_tool_is_never_asked() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("a fact"));
        end(&session, &[user("u", "hello")], Some(provider.clone())).await;
        assert!(provider.requests().is_empty());
        assert!(session.memories().await.is_empty());
    }

    #[tokio::test]
    async fn a_session_without_a_provider_writes_nothing() {
        let session = Session::new();
        end(&session, &working_turn(), None).await;
        assert!(session.memories().await.is_empty());
    }

    #[tokio::test]
    async fn a_provider_that_refuses_leaves_the_scope_alone() {
        let session = Session::new();
        let provider = Arc::new(Scripted::failing(ProviderError::Auth {
            message: "no key".into(),
        }));
        end(&session, &working_turn(), Some(provider)).await;
        assert!(session.memories().await.is_empty());
    }

    #[tokio::test]
    async fn an_empty_answer_writes_nothing() {
        let session = Session::new();
        let provider = Arc::new(Scripted::saying("  \n "));
        end(&session, &working_turn(), Some(provider)).await;
        assert!(session.memories().await.is_empty());
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
