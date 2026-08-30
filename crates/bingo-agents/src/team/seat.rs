//! Seating a project's team: every role `.bingo/team.json` declares becomes a
//! child of the root session opened in that project, once.
//!
//! Nothing is kept between runs. The roster is the session tree, so a role
//! already under this root is reopened — which is what carries its memory
//! into a resumed root — and the rest are created. Creation opens no turn: a
//! role idles at zero tokens until something is delivered to it.

use async_trait::async_trait;
use bingo_sdk::{
    Driver, Env, Hook, HookContext, HookMatcher, HookPoint, HostHandle, KernelError, OpenOptions,
    ParentLink, Phase, SessionId, SessionSelector, SessionSpec,
};

use crate::definition::Definition;
use crate::team::file::{self, Role, Team};
use crate::{library, names, note, watch};

/// Seats the team when a root session opens in a project that declares one.
#[derive(Debug, Clone)]
pub struct SeatHook {
    /// Where the definitions a role may name live. A hook's context carries
    /// no `Env`; this one is known when the plugin registers.
    env: Env,
}

impl SeatHook {
    pub fn new(env: Env) -> Self {
        Self { env }
    }

    /// Every role of this project under this root: the one already there
    /// reopened, the rest created.
    async fn seat(&self, cx: &HookContext) -> Result<(), KernelError> {
        let Some(team) = file::of(&cx.cwd)? else {
            return Ok(());
        };
        if team.roles.is_empty() || !is_root(&cx.host, &cx.session).await? {
            return Ok(());
        }
        let seated = names::children(&cx.host, &cx.session).await?;
        let definitions = library::load(&self.env, &cx.cwd);
        for role in &team.roles {
            // One role the host refuses — a key another process holds, a
            // provider with no credentials — is not the rest of the team.
            let outcome = match names::named(&seated, &role.name) {
                Some(live) => reopen(&cx.host, &live.id).await,
                None => create(&cx.host, spec(role, &definitions, &team, cx).await).await,
            };
            if let Err(error) = outcome {
                tracing::warn!(role = role.name, %error, "this role was not seated");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Hook for SeatHook {
    fn id(&self) -> &str {
        "agents.team"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Session],
            tool: None,
        }
    }

    /// A hook has nobody to answer to, so a team that cannot be read is a
    /// warning and a session with no roles, never a failure of the session.
    async fn on_session(&self, phase: Phase, cx: &HookContext) {
        if phase != Phase::Start {
            return;
        }
        if let Err(error) = self.seat(cx).await {
            tracing::warn!(session = %cx.session, %error, "this project's team was not seated");
        }
    }
}

/// A session with no parent is the one a person opened. A child seats
/// nobody: the team is its root's, and it is already in it.
async fn is_root(host: &HostHandle, session: &SessionId) -> Result<bool, KernelError> {
    Ok(names::own(host, session).await?.parent.is_none())
}

/// A persisted role, live again. The attachment is dropped at once: this
/// plugin watches a role's frames only when it spawned one to wait for.
async fn reopen(host: &HostHandle, role: &SessionId) -> Result<(), KernelError> {
    let selector = SessionSelector::ById { id: role.clone() };
    host.open(selector, watch::identity(), OpenOptions::default())
        .await
        .map(drop)
}

async fn create(host: &HostHandle, spec: SessionSpec) -> Result<(), KernelError> {
    host.open(
        SessionSelector::Create { spec },
        watch::identity(),
        OpenOptions::default(),
    )
    .await
    .map(drop)
}

/// What a role's session is: the role's own fields over its definition's, the
/// note and the team's norms above whichever system prompt won, the tool set
/// any child gets, and a key so a later seating knows it by more than its
/// title.
async fn spec(
    role: &Role,
    definitions: &[Definition],
    team: &Team,
    cx: &HookContext,
) -> SessionSpec {
    let definition = definitions
        .iter()
        .find(|d| Some(&d.name) == role.agent.as_ref());
    if definition.is_none() && role.agent.is_some() {
        tracing::warn!(
            role = role.name,
            "this role names a definition nobody wrote"
        );
    }
    let system = role
        .system
        .as_deref()
        .or(definition.map(|d| d.system.as_str()))
        .unwrap_or_default();
    SessionSpec {
        driver: Driver::Model,
        cwd: cx.cwd.clone(),
        key: Some(format!("agent/{}/{}", cx.session, role.name)),
        parent: Some(ParentLink {
            session: cx.session.clone(),
            item: None,
        }),
        title: Some(role.name.clone()),
        provider: role
            .provider
            .clone()
            .or_else(|| definition.and_then(|d| d.provider.clone())),
        model: role
            .model
            .clone()
            .or_else(|| definition.and_then(|d| d.model.clone())),
        system_extra: Some(note::system_extra(&team.system(system))),
        tools: crate::spawn::child_tools(
            &cx.host,
            role.tools
                .clone()
                .or_else(|| definition.and_then(|d| d.tools.clone())),
        )
        .await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Tree, hook_context};
    use bingo_sdk::SessionSpec;

    const TWO: &str = r#"{"roles":[
        { "name": "reviewer", "agent": "reviewer" },
        { "name": "scout", "system": "You look around." }
    ]}"#;

    /// A machine with a team file, and a fleet holding one root in it.
    struct Project {
        tree: Tree,
        fleet: Fleet,
        cwd: std::path::PathBuf,
    }

    impl Project {
        fn new(source: &str) -> Project {
            let tree = Tree::new();
            let cwd = tree.team("work", source);
            Project {
                tree,
                fleet: Fleet::default(),
                cwd,
            }
        }

        /// A machine with no team file at all.
        fn bare() -> Project {
            let tree = Tree::new();
            let cwd = tree.cwd();
            Project {
                tree,
                fleet: Fleet::default(),
                cwd,
            }
        }

        fn hook(&self) -> SeatHook {
            SeatHook::new(Env::rooted(self.tree.root()))
        }

        async fn opens(&self, session: &SessionId) {
            let cx = HookContext {
                cwd: self.cwd.clone(),
                ..hook_context(session, self.fleet.handle())
            };
            self.hook().on_session(Phase::Start, &cx).await;
        }
    }

    fn titles(specs: &[SessionSpec]) -> Vec<String> {
        specs
            .iter()
            .map(|spec| spec.title.clone().unwrap_or_default())
            .collect()
    }

    #[tokio::test]
    async fn a_root_seats_every_role_as_a_child_of_its_own() {
        let project = Project::new(TWO);
        let root = project.fleet.root();
        project.opens(&root).await;

        let specs = project.fleet.spawned();
        assert_eq!(titles(&specs), ["reviewer", "scout"]);
        let scout = &specs[1];
        assert_eq!(scout.key.as_deref(), Some(&*format!("agent/{root}/scout")));
        assert_eq!(scout.cwd, project.cwd);
        assert_eq!(scout.driver, Driver::Model);
        let parent = scout.parent.as_ref().expect("a role has a parent");
        assert_eq!(parent.session, root);
        assert_eq!(parent.item, None, "no tool call spawned a role");
        assert!(project.fleet.delivered().is_empty(), "a role idles");
    }

    #[tokio::test]
    async fn a_role_is_told_the_norms_before_its_own_system_prompt() {
        let project = Project::new(TWO);
        project
            .tree
            .write(&project.cwd.join(".bingo/team-norms.md"), "Ship small.\n");
        let root = project.fleet.root();
        project.opens(&root).await;

        let extra = project.fleet.spawned()[1]
            .system_extra
            .clone()
            .unwrap_or_default();
        assert!(extra.starts_with(note::NOTE), "{extra}");
        let norms = extra.find("Ship small.").expect("the norms");
        let system = extra.find("You look around.").expect("the role's own");
        assert!(norms < system, "{extra}");
        assert!(extra.contains("# Team norms"), "{extra}");
    }

    #[tokio::test]
    async fn a_definition_gives_a_role_what_it_does_not_declare_itself() {
        let project = Project::new(TWO);
        project.tree.write(
            &project.cwd.join(".bingo/agents/reviewer.md"),
            "---\nmodel: fake-2\nprovider: other\ntools: [Read]\n---\nYou review diffs.\n",
        );
        let root = project.fleet.root();
        project.opens(&root).await;

        let reviewer = &project.fleet.spawned()[0];
        assert_eq!(reviewer.model.as_deref(), Some("fake-2"));
        assert_eq!(reviewer.provider.as_deref(), Some("other"));
        assert_eq!(reviewer.tools.clone(), Some(vec!["Read".to_string()]));
        let extra = reviewer.system_extra.clone().unwrap_or_default();
        assert!(extra.ends_with("You review diffs."), "{extra}");
    }

    #[tokio::test]
    async fn a_role_the_root_already_holds_is_reopened_not_seated_twice() {
        let project = Project::new(TWO);
        let root = project.fleet.root();
        let reviewer = project.fleet.child(&root, "reviewer");
        project.opens(&root).await;

        assert_eq!(
            titles(&project.fleet.spawned()),
            ["scout"],
            "the one that was there is not created again"
        );
        assert_eq!(project.fleet.opened(), [reviewer]);
    }

    #[tokio::test]
    async fn a_child_session_seats_nobody() {
        let project = Project::new(TWO);
        let root = project.fleet.root();
        let reviewer = project.fleet.child(&root, "reviewer");
        project.opens(&reviewer).await;
        assert!(project.fleet.spawned().is_empty());
        assert!(project.fleet.opened().is_empty());
    }

    #[tokio::test]
    async fn a_project_that_declares_no_team_seats_nobody() {
        let project = Project::bare();
        let root = project.fleet.root();
        project.opens(&root).await;
        assert!(project.fleet.spawned().is_empty());
    }

    #[tokio::test]
    async fn a_team_that_cannot_be_read_seats_nobody_and_fails_no_session() {
        let project = Project::new("not json at all");
        let root = project.fleet.root();
        project.opens(&root).await;
        assert!(project.fleet.spawned().is_empty());
    }

    #[tokio::test]
    async fn nothing_happens_when_a_session_ends() {
        let project = Project::new(TWO);
        let root = project.fleet.root();
        let cx = HookContext {
            cwd: project.cwd.clone(),
            ..hook_context(&root, project.fleet.handle())
        };
        project.hook().on_session(Phase::End, &cx).await;
        assert!(project.fleet.spawned().is_empty());
    }

    #[test]
    fn it_asks_for_the_session_point_only() {
        let hook = SeatHook::new(Env::rooted("/nowhere"));
        assert_eq!(hook.id(), "agents.team");
        assert_eq!(hook.matcher().points, [HookPoint::Session]);
        assert!(hook.matcher().tool.is_none());
    }
}
