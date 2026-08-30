//! `.bingo/team.json`: the roles a project seats and the norms they share.
//! One pass over the nearest file, read again every time it is asked for, so
//! an edited team needs no restart.

use std::fmt::Display;
use std::path::{Path, PathBuf};

use bingo_sdk::{ErrorCode, KernelError};
use serde::Deserialize;

use crate::names;

/// The directory a project keeps its own configuration in.
const PROJECT: &str = ".bingo";

/// Where a project declares its team.
const FILE: &str = "team.json";

/// The norms file a team is given when it names none.
const NORMS: &str = "team-norms.md";

/// The heading the norms are given in a role's system prompt.
const HEADING: &str = "# Team norms";

/// A resident agent: a name, and whatever it does not leave to a definition.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Role {
    /// What the role is called: its session's title, and the name `@name`,
    /// `SendMessage` and `/team` know it by.
    pub name: String,
    /// A definition in `.bingo/agents`, whose system prompt, model, provider
    /// and tools the role takes unless it says otherwise.
    pub agent: Option<String>,
    /// The system prompt, written in the team file itself.
    pub system: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub tools: Option<Vec<String>>,
}

/// What a team file declares. A key this plugin does not know is ignored:
/// another plugin reads its own resources from the same file.
#[derive(Debug, Default, Deserialize)]
struct Document {
    #[serde(default)]
    roles: Vec<Role>,
    /// The norms file, relative to this file's own directory.
    norms: Option<String>,
}

/// A project's team as it is on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Team {
    pub roles: Vec<Role>,
    /// The norms every role is given, once the file has been read.
    pub norms: Option<String>,
}

impl Team {
    /// What a role's session is told before the note's business: the team's
    /// norms, then the role's own system prompt.
    pub fn system(&self, system: &str) -> String {
        let system = system.trim();
        match self.norms.as_deref().map(str::trim) {
            None => system.to_string(),
            Some(norms) if system.is_empty() => format!("{HEADING}\n\n{norms}"),
            Some(norms) => format!("{HEADING}\n\n{norms}\n\n{system}"),
        }
    }
}

/// Where a project would declare its team.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(PROJECT).join(FILE)
}

/// The team file a session in `cwd` belongs to: the nearest one at or above
/// it, as a definition's layer is found.
pub fn find(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().map(path_in).find(|path| path.is_file())
}

/// The team a session in `cwd` belongs to, norms and all; `None` when no
/// directory from here up declares one.
pub fn of(cwd: &Path) -> Result<Option<Team>, KernelError> {
    match find(cwd) {
        Some(path) => read(&path).map(Some),
        None => Ok(None),
    }
}

/// One team file, and the norms beside it.
fn read(path: &Path) -> Result<Team, KernelError> {
    let source = std::fs::read_to_string(path).map_err(|e| refused(path, e))?;
    let document = parse(&source).map_err(|e| refused(path, e.message))?;
    Ok(Team {
        roles: document.roles,
        norms: norms(path, document.norms.as_deref())?,
    })
}

/// What a source declares, with every role's name checked: a name the key or
/// the address could not carry is a mistake to report, not one to seat.
fn parse(source: &str) -> Result<Document, KernelError> {
    let document: Document = serde_json::from_str(source)
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e.to_string()))?;
    for role in &document.roles {
        names::check(&role.name)?;
    }
    Ok(document)
}

/// The norms every role is given: the file the team names, else the one
/// beside it when it is there. A file a team named and does not have is a
/// mistake; one nobody named is simply absent.
fn norms(path: &Path, named: Option<&str>) -> Result<Option<String>, KernelError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    match named {
        Some(named) => {
            let file = dir.join(named);
            std::fs::read_to_string(&file)
                .map(Some)
                .map_err(|e| refused(&file, e))
        }
        None => Ok(std::fs::read_to_string(dir.join(NORMS)).ok()),
    }
}

fn refused(path: &Path, what: impl Display) -> KernelError {
    KernelError::new(
        ErrorCode::InvalidInput,
        format!("{}: {what}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Tree;

    const FULL: &str = r#"{
        "roles": [
            { "name": "reviewer", "agent": "reviewer", "model": "fake-2",
              "provider": "other", "tools": ["Read"] },
            { "name": "scout", "system": "You look around." }
        ],
        "rooms": [{ "name": "design", "members": ["reviewer"] }],
        "somethingElse": 3
    }"#;

    fn team(tree: &Tree, source: &str) -> Result<Option<Team>, KernelError> {
        of(&tree.team("work", source))
    }

    #[test]
    fn every_field_a_role_declares_and_nothing_of_another_plugin_s() {
        let tree = Tree::new();
        let team = team(&tree, FULL).expect("a team").expect("a file");
        assert_eq!(team.roles.len(), 2);
        assert_eq!(
            team.roles[0],
            Role {
                name: "reviewer".into(),
                agent: Some("reviewer".into()),
                system: None,
                model: Some("fake-2".into()),
                provider: Some("other".into()),
                tools: Some(vec!["Read".into()]),
            }
        );
        assert_eq!(team.roles[1].system.as_deref(), Some("You look around."));
        assert_eq!(team.norms, None, "nobody wrote one");
    }

    #[test]
    fn a_file_that_declares_no_roles_seats_nobody() {
        let tree = Tree::new();
        let team = team(&tree, r#"{ "rooms": [] }"#)
            .expect("a team")
            .expect("a file");
        assert!(team.roles.is_empty());
    }

    #[test]
    fn a_role_without_a_usable_name_is_refused_by_name() {
        let tree = Tree::new();
        let error = team(&tree, r#"{"roles":[{"name":"two words"}]}"#).expect_err("a name");
        assert!(error.message.contains("is not a name"), "{error}");
        let error = team(&tree, r#"{"roles":[{"agent":"reviewer"}]}"#).expect_err("no name");
        assert!(error.message.contains("name"), "{error}");
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn a_file_that_is_not_json_says_where_it_is() {
        let tree = Tree::new();
        let error = team(&tree, "roles: []\n").expect_err("not json");
        assert!(error.message.contains("team.json"), "{error}");
    }

    #[test]
    fn the_norms_file_a_team_names() {
        let tree = Tree::new();
        let cwd = tree.team("work", r#"{"roles":[],"norms":"norms/ours.md"}"#);
        tree.write(&cwd.join(".bingo/norms/ours.md"), "Ship small.\n");
        assert_eq!(
            of(&cwd).expect("a team").expect("a file").norms.as_deref(),
            Some("Ship small.\n")
        );
    }

    #[test]
    fn the_norms_beside_the_file_when_it_names_none() {
        let tree = Tree::new();
        let cwd = tree.team("work", r#"{"roles":[]}"#);
        tree.write(&cwd.join(".bingo/team-norms.md"), "Say what you did.\n");
        assert_eq!(
            of(&cwd).expect("a team").expect("a file").norms.as_deref(),
            Some("Say what you did.\n")
        );
    }

    #[test]
    fn a_norms_file_a_team_named_and_does_not_have_is_a_mistake() {
        let tree = Tree::new();
        let error = team(&tree, r#"{"roles":[],"norms":"missing.md"}"#).expect_err("no file");
        assert!(error.message.contains("missing.md"), "{error}");
    }

    #[test]
    fn a_directory_with_no_team_file_anywhere_above_it_has_no_team() {
        let tree = Tree::new();
        assert_eq!(of(&tree.cwd()).expect("no team"), None);
        assert!(find(&tree.cwd()).is_none());
    }

    #[test]
    fn the_nearest_team_file_wins() {
        let tree = Tree::new();
        tree.team("work", r#"{"roles":[{"name":"reviewer"}]}"#);
        let inner = tree.team("work/crate", r#"{"roles":[{"name":"scout"}]}"#);
        let team = of(&inner).expect("a team").expect("a file");
        assert_eq!(team.roles[0].name, "scout");
        assert_eq!(find(&inner), Some(path_in(&inner)));
    }

    #[test]
    fn the_norms_come_before_the_system_prompt_and_alone_when_there_is_none() {
        let team = Team {
            roles: Vec::new(),
            norms: Some("Ship small.\n".into()),
        };
        assert_eq!(
            team.system("You review diffs.\n"),
            "# Team norms\n\nShip small.\n\nYou review diffs."
        );
        assert_eq!(team.system("  "), "# Team norms\n\nShip small.");
        assert_eq!(Team::default().system("You review."), "You review.");
    }
}
