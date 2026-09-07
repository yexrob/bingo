//! `agents.team`: the one door another plugin has onto `.bingo/team.json`.
//!
//! A project declares its team in one file, and the keys in it belong to
//! different plugins — `roles` and `norms` to this one, `rooms` to whoever
//! owns rooms. A plugin may not import another (ADR-0001), and it must not
//! keep a second parser for the file either, so the file crosses the way
//! ADR-0031 says a fact crosses a plugin line: a service under a key, met by
//! method and JSON. Not by a trait in the sdk — `team` is a feature noun the
//! kernel does not have, and never will.
//!
//! One method, `section`, answering one top-level key of the nearest file to
//! a directory. What that key *means* stays with whoever owns it: the value
//! goes back as a person wrote it and nothing here reads a word of it.
//!
//! No wire face. Every key of the file is reachable through this one method,
//! and what a person may write under a name nobody has claimed yet is not
//! this plugin's to hand an out-of-process stranger (ADR-0031 §3: crossing is
//! the owner's choice). One can be opened the day something asks.

use std::path::PathBuf;

use async_trait::async_trait;
use bingo_sdk::{ServiceError, WireService};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::team::file;

/// The key the service is registered under, and the one the manifest declares.
pub const TEAM: &str = "agents.team";

/// The method it speaks.
const SECTION: &str = "section";

/// What a caller asks for: a directory to look up from, and the key it owns.
#[derive(Debug, Deserialize)]
struct Asked {
    cwd: PathBuf,
    key: String,
}

/// The team file, answering one key at a time.
#[derive(Debug, Default, Clone, Copy)]
pub struct TeamFile;

#[async_trait]
impl WireService for TeamFile {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        if method != SECTION {
            return Err(ServiceError(format!(
                "{TEAM} speaks `{SECTION}`, not `{method}`"
            )));
        }
        let asked: Asked =
            serde_json::from_value(params).map_err(|e| ServiceError(e.to_string()))?;
        let section = file::section(&asked.cwd, &asked.key).map_err(|e| ServiceError(e.message))?;
        Ok(json!({ SECTION: section }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Tree;

    const FULL: &str = r#"{
        "roles": [{ "name": "reviewer" }],
        "rooms": [{ "name": "design", "members": ["reviewer"] }]
    }"#;

    async fn asked(cwd: &std::path::Path, key: &str) -> Result<Value, ServiceError> {
        TeamFile
            .call(SECTION, json!({ "cwd": cwd, "key": key }))
            .await
    }

    /// The recorded answer, which is another plugin's contract with this one:
    /// the key's value exactly as a person wrote it, under the method's name.
    #[tokio::test]
    async fn the_service_answers_one_key_as_a_person_wrote_it() {
        let tree = Tree::new();
        let cwd = tree.team("work", FULL);
        assert_eq!(
            asked(&cwd, "rooms").await.expect("it answers"),
            json!({ "section": [{ "name": "design", "members": ["reviewer"] }] })
        );
        assert_eq!(
            asked(&cwd, "roles").await.expect("it answers"),
            json!({ "section": [{ "name": "reviewer" }] }),
            "this plugin's own keys are not privileged"
        );
    }

    /// A project that declared nothing under that name, and one that declared
    /// no team at all, say the same thing: there is nothing there.
    #[tokio::test]
    async fn a_key_nobody_wrote_and_a_file_nobody_wrote_answer_alike() {
        let tree = Tree::new();
        let cwd = tree.team("work", r#"{ "roles": [] }"#);
        assert_eq!(
            asked(&cwd, "rooms").await.expect("it answers"),
            json!({ "section": null })
        );
        let bare = Tree::new();
        assert_eq!(
            asked(&bare.cwd(), "rooms").await.expect("it answers"),
            json!({ "section": null })
        );
    }

    /// A file that will not parse is one mistake reported once, by the plugin
    /// that owns the file, naming it.
    #[tokio::test]
    async fn a_file_that_will_not_parse_says_which_one() {
        let tree = Tree::new();
        let cwd = tree.team("work", "{ not json");
        let refused = asked(&cwd, "rooms").await.expect_err("a refusal");
        assert!(refused.to_string().contains("team.json"), "{refused}");
    }

    #[tokio::test]
    async fn a_method_this_service_does_not_speak_says_which_one_it_does() {
        let refused = TeamFile
            .call("roles", json!({}))
            .await
            .expect_err("a refusal");
        assert!(refused.to_string().contains(SECTION), "{refused}");
    }

    #[tokio::test]
    async fn params_that_name_no_directory_are_refused_rather_than_guessed() {
        assert!(
            TeamFile
                .call(SECTION, json!({ "key": "rooms" }))
                .await
                .is_err()
        );
    }
}
