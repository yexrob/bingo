//! The permission policy: five modes, an allow/deny/ask rule table, and a
//! decision that fails closed at every step.
//!
//! The plugin registers one `PermissionPolicy`; the kernel's gate asks it and
//! enforces the answer, resolving `Ask` through a person and handing back what
//! they decided. A rule accepted "for the session" comes back through
//! `on_verdict` and lives in memory only.

pub mod decide;
pub mod mode;
pub mod path;
pub mod rule;
pub mod scope;
pub mod session;
pub mod split;
pub mod url;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ConfigClaim, Contribution, Decision, Merge, PermissionPolicy, Plugin, PluginError,
    PluginManifest, PolicyInput, Registrar, SessionId, Verdict,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::decide::Request;
use crate::rule::{Call, Rules};
use crate::session::SessionRules;

pub use decide::decide;
pub use mode::{Mode, UnknownMode};
pub use rule::Rule;

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.permissions",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["policy:permissions"],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[("permissions", Merge::ByName)],
        schema,
    }),
};

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub permissions: Permissions,
}

/// A typo in a key here would quietly drop a deny list, so an unknown one is a
/// startup failure rather than a silence.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permissions {
    /// What happens when no rule decides. A runtime `--permission-mode` flag
    /// reaches the policy through this same key.
    #[serde(default)]
    pub default_mode: Option<Mode>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    /// Directories `acceptEdits` treats as part of the working tree.
    #[serde(default)]
    pub additional_directories: Vec<PathBuf>,
}

/// Registers the one permission policy.
#[derive(Debug, Default, Clone, Copy)]
pub struct PermissionsPlugin;

#[async_trait]
impl Plugin for PermissionsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let policy = PermissionsPolicy::new(settings.permissions, std::env::home_dir())?;
        registrar.add(Contribution::Policy(
            Arc::new(policy) as Arc<dyn PermissionPolicy>
        ));
        Ok(())
    }
}

#[derive(Debug)]
pub struct PermissionsPolicy {
    mode: Mode,
    rules: Rules,
    extra_dirs: Vec<PathBuf>,
    home: Option<PathBuf>,
    session: SessionRules,
}

impl PermissionsPolicy {
    /// A rule line the grammar cannot read stops the plugin: a deny rule
    /// dropped in silence is worse than a startup that says why.
    pub fn new(settings: Permissions, home: Option<PathBuf>) -> Result<Self, PluginError> {
        Ok(Self {
            mode: settings.default_mode.unwrap_or_default(),
            rules: Rules {
                allow: parse_table(&settings.allow)?,
                deny: parse_table(&settings.deny)?,
                ask: parse_table(&settings.ask)?,
            },
            extra_dirs: settings.additional_directories,
            home,
            session: SessionRules::default(),
        })
    }

    fn request<'a>(&'a self, input: PolicyInput<'a>) -> Request<'a> {
        Request {
            call: Call {
                name: &input.call.name,
                subjects: input.subjects,
            },
            traits: input.traits,
            confirm: input.confirm,
            mode: self.mode,
            cwd: input.cwd,
            home: self.home.as_deref(),
            extra_dirs: &self.extra_dirs,
        }
    }

    /// The configured tables, plus whatever this session's person accepted.
    fn rules_for(&self, session: &SessionId) -> Rules {
        let mut rules = self.rules.clone();
        rules.allow.extend(self.session.of(session));
        rules
    }
}

fn parse_table(lines: &[String]) -> Result<Vec<Rule>, PluginError> {
    lines
        .iter()
        .map(|line| {
            Rule::parse(line)
                .ok_or_else(|| PluginError::Config(format!("unreadable permission rule: {line}")))
        })
        .collect()
}

#[async_trait]
impl PermissionPolicy for PermissionsPolicy {
    fn id(&self) -> &str {
        MANIFEST.id
    }

    async fn decide(&self, input: PolicyInput<'_>) -> Decision {
        decide::decide(&self.request(input), &self.rules_for(input.session))
    }

    async fn on_verdict(&self, input: PolicyInput<'_>, verdict: &Verdict) {
        let Verdict::Allow { scope: Some(rule) } = verdict else {
            return;
        };
        if !self.session.install(input.session, rule) {
            tracing::warn!(
                rule,
                "a session scope this grammar cannot read installs nothing"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{Subject, ToolCall, ToolTraits};
    use serde_json::{Value, json};
    use std::path::Path;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            call_id: "call_1".into(),
            name: name.to_string(),
            input: Value::Null,
        }
    }

    fn bash_traits() -> ToolTraits {
        ToolTraits {
            trusted: true,
            ..ToolTraits::default()
        }
    }

    fn policy(settings: Value) -> PermissionsPolicy {
        let settings: Settings = serde_json::from_value(settings).expect("a readable slice");
        PermissionsPolicy::new(settings.permissions, Some(PathBuf::from("/home/user")))
            .expect("readable rules")
    }

    fn subjects(command: &str) -> Vec<Subject> {
        vec![Subject::Command {
            command: command.to_string(),
        }]
    }

    async fn ask_about(policy: &PermissionsPolicy, session: &SessionId, command: &str) -> Decision {
        let call = call("Bash");
        let traits = bash_traits();
        let subjects = subjects(command);
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

    #[test]
    fn the_manifest_says_what_it_provides() {
        assert_eq!(MANIFEST.id, "bingo.permissions");
        assert_eq!(MANIFEST.provides, ["policy:permissions"]);
        let claim = MANIFEST.config.expect("a config claim");
        assert_eq!(claim.keys.len(), 1);
        assert_eq!(claim.keys[0].0, "permissions");
    }

    #[test]
    fn the_plugin_registers_one_policy() {
        let mut registrar = Registrar::new("bingo.permissions", json!({}));
        PermissionsPlugin
            .register(&mut registrar)
            .expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        assert!(matches!(contributions[0], Contribution::Policy(_)));
    }

    #[test]
    fn an_empty_slice_is_the_default_mode_and_no_rules() {
        let policy = policy(json!({}));
        assert_eq!(policy.mode, Mode::Default);
        assert!(policy.rules.allow.is_empty());
    }

    #[test]
    fn the_slice_carries_the_mode_the_rules_and_the_extra_directories() {
        let policy = policy(json!({
            "permissions": {
                "defaultMode": "acceptEdits",
                "allow": ["Bash(git:*)"],
                "deny": ["Bash(rm:*)"],
                "ask": ["Write"],
                "additionalDirectories": ["/scratch"],
            }
        }));
        assert_eq!(policy.mode, Mode::AcceptEdits);
        assert_eq!(policy.rules.allow.len(), 1);
        assert_eq!(policy.rules.deny.len(), 1);
        assert_eq!(policy.rules.ask.len(), 1);
        assert_eq!(policy.extra_dirs, [PathBuf::from("/scratch")]);
    }

    #[test]
    fn a_misspelled_key_is_a_startup_failure_not_a_silence() {
        let slice = json!({ "permissions": { "dney": ["Bash(rm:*)"] } });
        assert!(serde_json::from_value::<Settings>(slice).is_err());
    }

    #[test]
    fn an_unreadable_rule_is_a_startup_failure() {
        let settings = Permissions {
            deny: vec!["Bash(unclosed".to_string()],
            ..Permissions::default()
        };
        let err = PermissionsPolicy::new(settings, None).expect_err("refused");
        assert!(err.to_string().contains("Bash(unclosed"), "{err}");
    }

    #[tokio::test]
    async fn an_accepted_scope_silences_the_next_call_of_that_session_only() {
        let policy = policy(json!({}));
        let mine = SessionId::from_raw("ses_mine");
        let theirs = SessionId::from_raw("ses_theirs");

        let Decision::Ask { scope, .. } = ask_about(&policy, &mine, "cargo test").await else {
            panic!("a bash call asks by default");
        };
        let scope = scope.expect("a simple command has a scope");
        assert_eq!(scope, "Bash(cargo:*)");

        let call = call("Bash");
        let traits = bash_traits();
        let subjects = subjects("cargo test");
        policy
            .on_verdict(
                PolicyInput {
                    call: &call,
                    traits: &traits,
                    subjects: &subjects,
                    confirm: None,
                    session: &mine,
                    cwd: Path::new("/work/project"),
                },
                &Verdict::Allow { scope: Some(scope) },
            )
            .await;

        assert!(matches!(
            ask_about(&policy, &mine, "cargo build").await,
            Decision::Allow { .. }
        ));
        assert!(
            matches!(
                ask_about(&policy, &theirs, "cargo build").await,
                Decision::Ask { .. }
            ),
            "another session never inherits the answer"
        );
    }

    #[tokio::test]
    async fn a_denial_installs_nothing() {
        let policy = policy(json!({}));
        let session = SessionId::from_raw("ses_mine");
        let call = call("Bash");
        let traits = bash_traits();
        let subjects = subjects("cargo test");
        policy
            .on_verdict(
                PolicyInput {
                    call: &call,
                    traits: &traits,
                    subjects: &subjects,
                    confirm: None,
                    session: &session,
                    cwd: Path::new("/work/project"),
                },
                &Verdict::Deny { feedback: None },
            )
            .await;
        assert!(matches!(
            ask_about(&policy, &session, "cargo build").await,
            Decision::Ask { .. }
        ));
    }
}
