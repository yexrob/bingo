//! What ADR-0031 opened: a service, met by key and method rather than by
//! type, over real child processes.
//!
//! Two plugins are installed from the one example: `store`, which declares
//! `kv`, and `caller`, which declares nothing and only asks. Every call the
//! caller makes goes out of its process, through the host's registry, and into
//! the store's — there is no pipe between them.

use std::sync::Arc;

use bingo_plugin_rpc::Manager;
use bingo_sdk::{ServiceHandle, Tool};
use serde_json::{Value, json};

use crate::harness::{Started, call, said, started_with};

/// The pair: one plugin that serves `kv`, one that only calls it.
async fn paired() -> Started {
    started_with(&[("store", &[]), ("caller", &["--no-service"])]).await
}

/// The tool a named plugin offers; both stubs offer one each.
async fn tool_of(manager: &Manager, plugin: &str) -> Arc<dyn Tool> {
    let name = format!("plugin__{plugin}__echo");
    manager
        .tools()
        .await
        .into_iter()
        .find(|tool| tool.spec().name == name)
        .unwrap_or_else(|| panic!("{plugin} offers a tool"))
}

/// What the caller's process got back when it asked the host for one call:
/// the answer, or the host's refusal in its own words.
async fn asks(started: &Started, plugin: &str, asked: Value) -> String {
    let tool = tool_of(&started.manager, plugin).await;
    let (_, answered) = call(&tool, json!({ "call": asked }), started.project.path()).await;
    said(&answered.expect("the tool answered"))
}

/// The exit criterion of ADR-0031: two processes pair on one service, and the
/// value one of them wrote through the host is in the other's own memory.
#[tokio::test]
async fn two_processes_pair_on_a_service_through_the_host() {
    let started = paired().await;
    let wrote = asks(
        &started,
        "caller",
        json!({ "key": "kv", "method": "set", "params": { "key": "greeting", "value": "hello" } }),
    )
    .await;
    assert_eq!(wrote, "null", "a set answers nothing, which is an answer");

    let read = asks(
        &started,
        "store",
        json!({ "key": "kv", "method": "get", "params": { "key": "greeting" } }),
    )
    .await;
    assert_eq!(
        read, "hello",
        "the store answered out of the map the caller's write landed in"
    );
    started.manager.shutdown().await;
}

/// And the round trip the other way: the caller reads back what it wrote,
/// through the host, without ever holding the store's map.
#[tokio::test]
async fn a_caller_reads_back_what_it_wrote_across_two_processes() {
    let started = paired().await;
    asks(
        &started,
        "caller",
        json!({ "key": "kv", "method": "set", "params": { "key": "one", "value": "1" } }),
    )
    .await;
    let read = asks(
        &started,
        "caller",
        json!({ "key": "kv", "method": "get", "params": { "key": "one" } }),
    )
    .await;
    assert_eq!(read, "1");
    started.manager.shutdown().await;
}

/// An in-process consumer finds an external service through the one lookup and
/// cannot tell that the implementation is a process (ADR-0031 §2).
#[tokio::test]
async fn an_in_process_consumer_reaches_an_external_service_by_key() {
    let started = paired().await;
    let kv = started
        .host
        .service::<ServiceHandle>("kv")
        .expect("a declared service is in the registry, under its key");
    kv.call("set", json!({ "key": "here", "value": "in process" }))
        .await
        .expect("the call crossed");
    assert_eq!(
        kv.call("get", json!({ "key": "here" }))
            .await
            .expect("and so did the read"),
        json!("in process")
    );
    started.manager.shutdown().await;
}

/// A key nobody holds is refused in words, from the far side of the pipe.
#[tokio::test]
async fn a_call_to_a_service_nobody_serves_is_refused_in_words() {
    let started = paired().await;
    let refused = asks(
        &started,
        "caller",
        json!({ "key": "ledger", "method": "get", "params": {} }),
    )
    .await;
    assert_eq!(refused, "no service is registered under ledger");
    started.manager.shutdown().await;
}

/// A method the declaration never named is answered with the set the service
/// speaks, and never crosses to the process at all (ADR-0031 §5).
#[tokio::test]
async fn an_unknown_method_is_answered_with_the_set_the_service_speaks() {
    let started = paired().await;
    let refused = asks(
        &started,
        "caller",
        json!({ "key": "kv", "method": "drop", "params": {} }),
    )
    .await;
    assert_eq!(
        refused,
        "store: the service kv does not speak drop; it speaks get, set"
    );
    started.manager.shutdown().await;
}

/// One key has one owner: a second plugin declaring `kv` is told, once, that
/// it is not available — and the first plugin's service is untouched.
#[tokio::test]
async fn a_second_plugin_claiming_the_same_key_is_reported_and_the_first_keeps_it() {
    let started = started_with(&[("store", &[]), ("twin", &[])]).await;
    let said = started.manager.notices().drain();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].code, "SERVICE_TAKEN");
    assert!(said[0].text.contains("kv"), "{}", said[0].text);
    assert!(
        started.host.service::<ServiceHandle>("kv").is_some(),
        "the key is still served by whoever claimed it first"
    );
    started.manager.shutdown().await;
}
