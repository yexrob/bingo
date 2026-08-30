//! `/permission [mode]`: what this session does when no rule decides.
//!
//! The mode a person names here is an answer, not a setting: it lives beside
//! the rules they accepted, in memory, for this session only — another session
//! and a sub-agent keep the configured mode, and no file is written (ADR-0008).
//!
//! It is not a second gate. Every mode may be named, `bypassPermissions`
//! included, because the ladder above the mode does not move: a deny rule, an
//! ask rule, a write into a sensitive directory and a tool's own confirmation
//! still stop the call in any mode.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, KernelError,
    SessionId, View,
};

use crate::PermissionsPolicy;
use crate::mode::Mode;

#[derive(Debug)]
pub struct PermissionCommand {
    policy: Arc<PermissionsPolicy>,
}

impl PermissionCommand {
    pub fn new(policy: Arc<PermissionsPolicy>) -> Self {
        Self { policy }
    }

    /// The mode this session runs in, and the five it could run in.
    fn listing(&self, session: &SessionId) -> String {
        let width = Mode::ALL
            .iter()
            .map(|mode| mode.as_str().len())
            .max()
            .unwrap_or(0);
        let modes = Mode::ALL
            .map(|mode| format!("  {:width$}  {}", mode.as_str(), mode.meaning()))
            .join("\n");
        format!(
            "permission mode: {}\n\n{modes}",
            self.policy.mode_for(session)
        )
    }
}

#[async_trait]
impl Command for PermissionCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "permission".into(),
            aliases: vec!["permissions".into()],
            hint: "[mode]".into(),
            args: ArgSpec::Free {
                hint: "default | acceptEdits | plan | bypassPermissions | dontAsk".into(),
            },
            // Reading and setting the mode touch nothing the turn is using; the
            // mode the next call is decided against is the one set last.
            instant: true,
            family: "session".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let asked = args.trim();
        if asked.is_empty() {
            return Ok(CommandOutcome::View {
                view: View::Text {
                    text: self.listing(&cx.session),
                },
            });
        }
        let mode: Mode = asked.parse().map_err(|unknown: crate::UnknownMode| {
            KernelError::new(ErrorCode::InvalidInput, unknown.to_string())
        })?;
        self.policy.choose_mode(&cx.session, mode);
        // Read back rather than echo: the message says what the next call will
        // be decided against.
        Ok(CommandOutcome::Applied {
            message: Some(format!(
                "permission mode: {}",
                self.policy.mode_for(&cx.session)
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::path::{Path, PathBuf};

    use bingo_sdk::{
        Attachment, Catalog, CatalogKind, ClientIdentity, CloseReason, Decision, GatewayStream,
        HostApi, HostHandle, OpenOptions, PermissionPolicy, PolicyInput, SessionFilter,
        SessionSelector, SessionSummary, Subject, ToolCall, ToolTraits,
    };
    use serde_json::{Value, json};

    use crate::Settings;

    /// A command context reads its session, its directory and a host;
    /// `/permission` asks the host nothing, so every answer here would be a bug.
    struct UnusedHost;

    #[async_trait]
    impl HostApi for UnusedHost {
        async fn sessions(
            &self,
            _filter: SessionFilter,
        ) -> Result<Vec<SessionSummary>, KernelError> {
            unreachable!("/permission reads no session list")
        }

        async fn open(
            &self,
            _selector: SessionSelector,
            _who: ClientIdentity,
            _options: OpenOptions,
        ) -> Result<Attachment, KernelError> {
            unreachable!("/permission opens no session")
        }

        async fn close(
            &self,
            _session: &SessionId,
            _reason: CloseReason,
        ) -> Result<(), KernelError> {
            unreachable!("/permission closes no session")
        }

        async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
            unreachable!("/permission deletes no session")
        }

        async fn deliver(
            &self,
            _to: &SessionId,
            _intent: bingo_sdk::IntentId,
            _input: bingo_sdk::Input,
            _delivery: bingo_sdk::Delivery,
        ) -> Result<(), KernelError> {
            unreachable!("this double delivers nothing")
        }

        async fn extend(
            &self,
            _session: &SessionId,
            _plugin: &str,
            _kind: &str,
            _payload: serde_json::Value,
        ) -> Result<(), KernelError> {
            unreachable!("this double extends nothing")
        }

        async fn catalog(&self, _kind: CatalogKind) -> Result<Catalog, KernelError> {
            unreachable!("/permission reads no catalog")
        }

        fn gateway_events(&self) -> GatewayStream {
            unreachable!("/permission watches no gateway")
        }

        fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
            None
        }
    }

    fn context(session: &str) -> CommandContext {
        CommandContext {
            session: SessionId::from_raw(session),
            cwd: PathBuf::from("/work/project"),
            host: HostHandle(Arc::new(UnusedHost)),
        }
    }

    fn policy(settings: Value) -> Arc<PermissionsPolicy> {
        let settings: Settings = serde_json::from_value(settings).expect("a readable slice");
        Arc::new(
            PermissionsPolicy::new(settings.permissions, Some(PathBuf::from("/home/user")))
                .expect("readable rules"),
        )
    }

    /// What the gate would decide about an edit in the working tree, which is
    /// the call `acceptEdits` is about.
    async fn edit_in(policy: &PermissionsPolicy, session: &SessionId) -> Decision {
        let call = ToolCall {
            call_id: "call_1".into(),
            name: "Edit".into(),
            input: Value::Null,
        };
        let traits = ToolTraits {
            trusted: true,
            edit: true,
            ..ToolTraits::default()
        };
        let subjects = vec![Subject::Path {
            path: PathBuf::from("/work/project/src/main.rs"),
        }];
        policy
            .decide(PolicyInput {
                call: &call,
                traits: &traits,
                subjects: &subjects,
                confirm: None,
                session,
                cwd: Path::new("/work/project"),
            })
            .await
    }

    async fn typed(command: &PermissionCommand, args: &str, session: &str) -> CommandOutcome {
        command
            .run(args, &context(session))
            .await
            .expect("a mode this crate knows")
    }

    #[tokio::test]
    async fn a_mode_chosen_in_one_session_decides_that_session_only() {
        let policy = policy(json!({}));
        let command = PermissionCommand::new(Arc::clone(&policy));
        let mine = SessionId::from_raw("ses_mine");
        let theirs = SessionId::from_raw("ses_theirs");

        assert!(
            matches!(edit_in(&policy, &mine).await, Decision::Ask { .. }),
            "an edit asks in the default mode"
        );

        let outcome = typed(&command, "acceptEdits", "ses_mine").await;
        assert_eq!(
            outcome,
            CommandOutcome::Applied {
                message: Some("permission mode: acceptEdits".into())
            }
        );

        assert!(matches!(
            edit_in(&policy, &mine).await,
            Decision::Allow { .. }
        ));
        assert!(
            matches!(edit_in(&policy, &theirs).await, Decision::Ask { .. }),
            "another session never inherits the mode"
        );
    }

    #[tokio::test]
    async fn the_session_s_mode_leaves_the_configured_one_alone() {
        let policy = policy(json!({ "permissions": { "defaultMode": "plan" } }));
        let command = PermissionCommand::new(Arc::clone(&policy));
        typed(&command, "acceptEdits", "ses_mine").await;

        assert_eq!(policy.mode, Mode::Plan, "the settings were written to");
        assert_eq!(
            policy.mode_for(&SessionId::from_raw("ses_fresh")),
            Mode::Plan,
            "a session that chose nothing runs configured"
        );
    }

    #[tokio::test]
    async fn no_argument_names_this_session_s_mode_and_the_five_to_choose_from() {
        let policy = policy(json!({}));
        let command = PermissionCommand::new(Arc::clone(&policy));
        typed(&command, "plan", "ses_mine").await;

        let CommandOutcome::View {
            view: View::Text { text },
        } = typed(&command, "  ", "ses_mine").await
        else {
            panic!("a listing is a text view");
        };
        assert!(
            text.starts_with("permission mode: plan\n"),
            "the session's own mode comes first: {text}"
        );
        for mode in Mode::ALL {
            assert!(
                text.contains(mode.as_str()),
                "{mode} is not offered: {text}"
            );
            assert!(text.contains(mode.meaning()), "{mode} is unexplained");
        }
    }

    #[tokio::test]
    async fn a_mode_nobody_defined_is_refused_and_changes_nothing() {
        let policy = policy(json!({}));
        let command = PermissionCommand::new(Arc::clone(&policy));
        let session = SessionId::from_raw("ses_mine");

        let error = command
            .run("yolo", &context("ses_mine"))
            .await
            .expect_err("not a mode");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("bypassPermissions"), "{error}");
        assert_eq!(policy.mode_for(&session), Mode::Default);
    }

    #[tokio::test]
    async fn bypassing_the_gate_is_a_mode_like_any_other() {
        let policy = policy(json!({ "permissions": { "deny": ["Edit"] } }));
        let command = PermissionCommand::new(Arc::clone(&policy));
        let session = SessionId::from_raw("ses_mine");
        typed(&command, "bypassPermissions", "ses_mine").await;

        assert_eq!(policy.mode_for(&session), Mode::BypassPermissions);
        assert!(
            matches!(edit_in(&policy, &session).await, Decision::Deny { .. }),
            "a deny rule stands above every mode"
        );
    }

    #[test]
    fn the_spec_runs_now_and_takes_a_mode() {
        let spec = PermissionCommand::new(policy(json!({}))).spec();
        assert_eq!(spec.name, "permission");
        assert_eq!(spec.aliases, ["permissions"]);
        assert!(spec.instant, "reading a mode never waits for a turn");
        assert_eq!(spec.family, "session");
        let ArgSpec::Free { hint } = spec.args else {
            panic!("a mode is free text");
        };
        for mode in Mode::ALL {
            assert!(hint.contains(mode.as_str()), "{mode} is not in the hint");
        }
    }
}
