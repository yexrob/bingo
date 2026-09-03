//! The claimed slice: `mcpServers`, keyed by name, and `disabledMcpServers`.
//!
//! A configured server reaches the rest of the plugin as a [`Server`], which
//! can only be one transport with the fields that transport needs — the entry
//! on disk is checked once, here, and never carried around half-read. An
//! unknown or misplaced field is a startup failure: a server that silently
//! never dials is worse than one that says why.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use schemars::{JsonSchema, SchemaGenerator};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, Server>,
    /// Names that start disabled; a disabled server is never dialled.
    #[serde(default)]
    pub disabled_mcp_servers: Vec<String>,
}

/// One configured server, in the transport it speaks.
#[derive(Clone, PartialEq, Eq)]
pub enum Server {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// A child's environment and an HTTP server's headers are where a person keeps
/// their tokens, so the values never print — only the names they were given.
impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Server::Stdio {
                command, args, env, ..
            } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field("env", &Names(env))
                .finish_non_exhaustive(),
            Server::Http { url, headers } => f
                .debug_struct("Http")
                .field("url", url)
                .field("headers", &Names(headers))
                .finish_non_exhaustive(),
        }
    }
}

struct Names<'a>(&'a BTreeMap<String, String>);

impl fmt::Debug for Names<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

/// A server entry as it is written, before it is known to be one transport.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    #[serde(rename = "type", default)]
    transport: Transport,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Transport {
    /// A child process speaking the protocol over its stdin and stdout.
    #[default]
    Stdio,
    /// A streamable-HTTP endpoint.
    Http,
}

impl Entry {
    fn into_server<E: serde::de::Error>(self) -> Result<Server, E> {
        match self.transport {
            Transport::Stdio => self.into_stdio(),
            Transport::Http => self.into_http(),
        }
    }

    fn into_stdio<E: serde::de::Error>(self) -> Result<Server, E> {
        let command = self
            .command
            .ok_or_else(|| E::custom("a stdio server needs a command"))?;
        if self.url.is_some() || !self.headers.is_empty() {
            return Err(E::custom("url and headers belong to an http server"));
        }
        Ok(Server::Stdio {
            command,
            args: self.args,
            env: self.env,
            cwd: self.cwd,
        })
    }

    fn into_http<E: serde::de::Error>(self) -> Result<Server, E> {
        let url = self
            .url
            .ok_or_else(|| E::custom("an http server needs a url"))?;
        if self.command.is_some() || !self.args.is_empty() || !self.env.is_empty() {
            return Err(E::custom("command, args and env belong to a stdio server"));
        }
        Ok(Server::Http {
            url,
            headers: self.headers,
        })
    }
}

impl<'de> Deserialize<'de> for Server {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Entry::deserialize(deserializer)?.into_server::<D::Error>()
    }
}

/// One configured server written back out in the shape it was read in — an
/// [`Entry`], the thing on disk.
///
/// Somebody else may have to dial these servers instead of us (an ACP agent
/// handed them at `session/new`, ADR-0036 §4), and what they must be handed is
/// the row a person wrote: their own command, their own env, their own
/// headers. Written here, beside the reader, so the two spellings of the entry
/// cannot drift apart — the round trip is a test.
pub fn row(server: &Server) -> Value {
    match server {
        Server::Stdio {
            command,
            args,
            env,
            cwd,
        } => json!({
            "type": "stdio",
            "command": command,
            "args": args,
            "env": env,
            "cwd": cwd,
        }),
        Server::Http { url, headers } => json!({
            "type": "http",
            "url": url,
            "headers": headers,
        }),
    }
}

/// The schema a client reads is the entry's: the two transports are one shape
/// on disk, told apart by `type`.
impl JsonSchema for Server {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        Entry::schema_name()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        Entry::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> schemars::Schema {
        Entry::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: serde_json::Value) -> Result<Settings, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn server(value: serde_json::Value) -> Result<Server, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn an_empty_slice_configures_no_servers() {
        let settings = settings(json!({})).expect("a readable slice");
        assert!(settings.mcp_servers.is_empty());
        assert!(settings.disabled_mcp_servers.is_empty());
    }

    #[test]
    fn an_entry_without_a_type_is_a_child_process() {
        let parsed = server(json!({
            "command": "npx",
            "args": ["-y", "mcp-server-files"],
            "env": { "TOKEN": "s3cret" },
            "cwd": "/work",
        }))
        .expect("a readable entry");
        assert_eq!(
            parsed,
            Server::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "mcp-server-files".into()],
                env: BTreeMap::from([("TOKEN".to_string(), "s3cret".to_string())]),
                cwd: Some(PathBuf::from("/work")),
            }
        );
    }

    #[test]
    fn an_http_entry_carries_its_url_and_headers() {
        let parsed = server(json!({
            "type": "http",
            "url": "https://mcp.example.com/mcp",
            "headers": { "Authorization": "Bearer s3cret" },
        }))
        .expect("a readable entry");
        assert_eq!(
            parsed,
            Server::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: BTreeMap::from([(
                    "Authorization".to_string(),
                    "Bearer s3cret".to_string()
                )]),
            }
        );
    }

    #[test]
    fn a_transport_nobody_implements_is_a_config_error() {
        let error = server(json!({ "type": "sse", "url": "http://localhost:8000/sse" }))
            .expect_err("refused");
        assert!(error.to_string().contains("sse"), "{error}");
        assert!(error.to_string().contains("stdio"), "{error}");
    }

    #[test]
    fn a_misspelled_field_is_a_config_error() {
        let error = server(json!({ "command": "npx", "arguments": ["-y"] })).expect_err("refused");
        assert!(error.to_string().contains("arguments"), "{error}");
    }

    #[test]
    fn a_stdio_entry_without_a_command_says_so() {
        let error = server(json!({ "args": ["-y"] })).expect_err("refused");
        assert!(error.to_string().contains("needs a command"), "{error}");
    }

    #[test]
    fn an_http_entry_without_a_url_says_so() {
        let error = server(json!({ "type": "http" })).expect_err("refused");
        assert!(error.to_string().contains("needs a url"), "{error}");
    }

    #[test]
    fn a_field_of_the_other_transport_is_refused_rather_than_ignored() {
        let error =
            server(json!({ "command": "npx", "url": "http://localhost" })).expect_err("refused");
        assert!(error.to_string().contains("http server"), "{error}");

        let error = server(json!({ "type": "http", "url": "http://localhost", "command": "npx" }))
            .expect_err("refused");
        assert!(error.to_string().contains("stdio server"), "{error}");
    }

    #[test]
    fn the_slice_reads_both_claimed_keys() {
        let settings = settings(json!({
            "mcpServers": { "files": { "command": "npx" } },
            "disabledMcpServers": ["files"],
        }))
        .expect("a readable slice");
        assert_eq!(settings.mcp_servers.len(), 1);
        assert_eq!(settings.disabled_mcp_servers, ["files"]);
    }

    /// The row written out is the row that was read: whatever a person put on
    /// disk comes back out of [`row`] and parses to the same server.
    #[test]
    fn a_row_written_back_out_reads_as_the_server_it_came_from() {
        for written in [
            json!({ "command": "npx", "args": ["-y", "files"], "env": { "TOKEN": "s3cret" }, "cwd": "/work" }),
            json!({ "type": "http", "url": "https://mcp.example.com/mcp",
                    "headers": { "Authorization": "Bearer s3cret" } }),
        ] {
            let parsed = server(written).expect("a readable entry");
            assert_eq!(
                server(row(&parsed)).expect("and the row it writes reads back"),
                parsed
            );
        }
    }

    /// A row is handed to whoever dials the server, so it carries what dialling
    /// it takes — the values, not only their names.
    #[test]
    fn a_row_carries_what_dialling_the_server_takes() {
        let parsed = server(json!({ "command": "npx", "env": { "TOKEN": "s3cret" } }))
            .expect("a readable entry");
        assert_eq!(row(&parsed)["env"]["TOKEN"], json!("s3cret"));
        assert_eq!(row(&parsed)["type"], json!("stdio"));
    }

    #[test]
    fn a_server_prints_the_names_of_its_secrets_and_not_their_values() {
        let parsed = server(json!({
            "command": "npx",
            "env": { "GITHUB_TOKEN": "ghp_live" },
        }))
        .expect("a readable entry");
        let printed = format!("{parsed:?}");
        assert!(printed.contains("GITHUB_TOKEN"), "{printed}");
        assert!(!printed.contains("ghp_live"), "{printed}");

        let parsed = server(json!({
            "type": "http",
            "url": "https://mcp.example.com/mcp",
            "headers": { "Authorization": "Bearer ghp_live" },
        }))
        .expect("a readable entry");
        let printed = format!("{parsed:?}");
        assert!(printed.contains("Authorization"), "{printed}");
        assert!(!printed.contains("ghp_live"), "{printed}");
    }
}
