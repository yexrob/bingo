//! `mcp.servers`: the one door another plugin has onto the configured rows.
//!
//! A plugin cannot read a settings key another plugin claimed — the kernel
//! refuses a second claim on one key — and it must not keep a copy of the
//! shape either. So the rows cross the way ADR-0031 says a fact crosses a
//! plugin line: a service under a key, met by method and JSON.
//!
//! One method, `servers`, answering what [`Manager::rows`] holds now. Live,
//! not a snapshot: `/mcp disable files` takes a server out of the answer the
//! same moment it takes it out of bingo's own hands.
//!
//! The rows carry a person's own env and headers, which is where their tokens
//! live. That is the point — whoever asks is dialling those servers instead of
//! us (ADR-0036 §4) — and it is why the answer never reaches a log: nothing
//! here formats a row, and the wire face is `wire: Some`, in process, rather
//! than a face another process may call.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ServiceError, WireService};
use serde_json::{Value, json};

use crate::manager::Manager;

/// The key the service is registered under, and the one the manifest declares.
pub const SERVERS: &str = "mcp.servers";

/// The method it speaks.
const METHOD: &str = "servers";

pub struct Rows {
    manager: Arc<Manager>,
}

impl Rows {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl WireService for Rows {
    async fn call(&self, method: &str, _params: Value) -> Result<Value, ServiceError> {
        if method != METHOD {
            return Err(ServiceError(format!(
                "{SERVERS} speaks `{METHOD}`, not `{method}`"
            )));
        }
        Ok(json!({ METHOD: self.manager.rows().await }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Server;
    use std::collections::BTreeMap;

    fn manager(servers: Value, disabled: &[String]) -> Arc<Manager> {
        let servers: BTreeMap<String, Server> =
            serde_json::from_value(servers).expect("readable rows");
        Arc::new(Manager::new(servers, disabled, std::env::temp_dir()))
    }

    /// The recorded answer: the `mcpServers` key's own shape, keyed by name,
    /// with what dialling each server takes.
    #[tokio::test]
    async fn the_service_answers_the_rows_as_a_person_wrote_them() {
        let rows = Rows::new(manager(
            json!({
                "files": { "command": "npx", "args": ["-y", "files"], "env": { "TOKEN": "s3cret" } },
                "remote": { "type": "http", "url": "https://mcp.example.com/mcp" }
            }),
            &[],
        ));
        let answered = rows.call(METHOD, Value::Null).await.expect("it answers");
        assert_eq!(
            answered["servers"]["files"],
            json!({ "type": "stdio", "command": "npx", "args": ["-y", "files"],
                    "env": { "TOKEN": "s3cret" }, "cwd": null })
        );
        assert_eq!(
            answered["servers"]["remote"],
            json!({ "type": "http", "url": "https://mcp.example.com/mcp", "headers": {} })
        );
    }

    /// A server a person switched off is not a row anyone is handed: bingo
    /// would not dial it either.
    #[tokio::test]
    async fn a_server_that_is_switched_off_is_not_answered() {
        let manager = manager(
            json!({ "files": { "command": "npx" }, "web": { "command": "npx" } }),
            &["web".to_string()],
        );
        let rows = Rows::new(Arc::clone(&manager));
        let answered = rows.call(METHOD, Value::Null).await.expect("it answers");
        assert!(answered["servers"]["files"].is_object());
        assert!(answered["servers"].get("web").is_none());

        manager.disable("files").await;
        let answered = rows.call(METHOD, Value::Null).await.expect("it answers");
        assert_eq!(
            answered["servers"],
            json!({}),
            "the answer is live, not a snapshot taken at boot"
        );
    }

    #[tokio::test]
    async fn a_method_this_service_does_not_speak_says_which_one_it_does() {
        let refused = Rows::new(manager(json!({}), &[]))
            .call("tools", Value::Null)
            .await
            .expect_err("a refusal");
        assert!(refused.to_string().contains("servers"), "{refused}");
    }
}
