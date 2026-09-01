//! A process is allowed to refuse, and allowed to die (ADR-0015 §5): what the
//! host does with either.

use std::collections::BTreeMap;

use bingo_plugin_rpc::{Manager, log_path};
use bingo_sdk::{Env, Level, ToolError};
use serde_json::json;

use crate::harness::{call, only_tool, respawned, said, started, started_with};

/// Nothing a process says about itself is believed beyond what the
/// declaration must carry: a placement this host cannot read refuses the whole
/// handshake, in words, rather than being guessed at.
#[tokio::test]
async fn a_declaration_this_host_cannot_read_refuses_the_handshake_in_words() {
    let started = started_with(&[("stub", &["--placement", "whenever"])]).await;
    let (_, text) = started.heard("PLUGIN_UNAVAILABLE").await;
    assert!(text.contains("whenever"), "{text}");
    assert!(started.manager.contributors().await.is_empty());
    assert!(
        started.manager.tools().await.is_empty(),
        "a plugin whose declaration is unreadable contributes nothing at all"
    );
    started.manager.shutdown().await;
}

#[tokio::test]
async fn a_plugin_s_stderr_goes_to_a_log_under_the_data_directory() {
    let (manager, home, _project) = started(&[]).await;
    let log = log_path(&Env::rooted(home.path()).data_dir, "stub");
    assert!(log.exists(), "{} was never opened", log.display());
    manager.shutdown().await;
}

/// The exit criterion of ADR-0015 §5: a dead process answers nothing, says so
/// once, and is back on the next read. The death is said through the host by
/// the one drain, not by the call that found it (ADR-0033 §4).
#[tokio::test]
async fn a_killed_process_leaves_one_notice_empty_sources_and_a_working_respawn() {
    let started = started_with(&[("stub", &[])]).await;
    let manager = &started.manager;
    let tool = only_tool(manager).await;

    let (_, answered) = call(&tool, json!({ "die": true }), started.project.path()).await;
    let error = answered.expect_err("a process that ended answers nothing");
    assert!(
        matches!(&error, ToolError::Failed(why) if why.starts_with("stub: ")),
        "{error}"
    );
    let (level, text) = started.heard("PLUGIN_DIED").await;
    assert_eq!(level, Level::Warn);
    assert!(text.contains("stub"), "{text}");
    assert_eq!(
        started
            .listener
            .all()
            .iter()
            .filter(|(_, code, _)| code == "PLUGIN_DIED")
            .count(),
        1,
        "one death is one notice"
    );

    assert!(
        manager.tools().await.is_empty(),
        "a dead plugin's source answers nothing"
    );
    assert_eq!(respawned(manager).await.len(), 1, "and it comes back");

    let tool = only_tool(manager).await;
    let (_, answered) = call(
        &tool,
        json!({ "text": "alive again" }),
        started.project.path(),
    )
    .await;
    assert_eq!(said(&answered.expect("an output")), "alive again");
    manager.shutdown().await;
}

#[tokio::test]
async fn an_unknown_protocol_major_refuses_the_handshake_with_a_notice() {
    let started = started_with(&[("stub", &["--protocol", "99"])]).await;
    let (_, text) = started.heard("PLUGIN_UNAVAILABLE").await;
    assert!(text.contains("protocol 99"), "{text}");
    assert!(
        started.manager.tools().await.is_empty(),
        "a plugin whose wire is unknown contributes nothing"
    );
    started.manager.shutdown().await;
}

#[tokio::test]
async fn a_plugin_whose_command_is_gone_is_reported_and_contributes_nothing() {
    let home = tempfile::tempdir().expect("a home");
    let root = home.path().join(".bingo/plugins/missing");
    std::fs::create_dir_all(&root).expect("a plugin directory");
    std::fs::write(
        root.join("plugin.json"),
        json!({
            "name": "missing",
            "version": "0.1.0",
            "entry": { "command": "bingo-no-such-plugin" }
        })
        .to_string(),
    )
    .expect("a manifest");
    let project = tempfile::tempdir().expect("a project");
    // Over a host with nowhere for a notice to land: the drain keeps the line
    // rather than losing it, so it is still on the channel to be read here.
    let manager = Manager::new(Env::rooted(home.path()), BTreeMap::new());
    manager
        .start(project.path(), bingo_sdk::testing::ServiceHost::handle())
        .await;
    let said = manager.notices().drain();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].code, "PLUGIN_UNAVAILABLE");
    assert!(manager.tools().await.is_empty());
    assert!(manager.commands().await.is_empty());
    manager.shutdown().await;
}
