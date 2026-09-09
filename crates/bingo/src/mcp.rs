//! `bingo mcp list | get | add | remove | login | logout`: the headless twin
//! of `/mcp` (ADR-0050 §4).
//!
//! Every verb here runs before a kernel exists, as `provider add` does: what
//! `add` and `remove` write is what the *next* run reads, and `login` needs
//! the servers rather than a session. Two files are touched, each for what it
//! is — a server's row goes to the *user* settings layer, never to a project
//! file somebody commits; its token goes to `auth.json` at 0600, through the
//! store and nowhere else.
//!
//! Stdout carries the answer and nothing else. A header value and an
//! environment value are where a person keeps their secrets, so `get` prints
//! the names they were given under and never what is in them.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use bingo_auth_oauth::CredentialStore;
use bingo_core::settings::{self, Claim, Layer};
use bingo_mcp::{McpPlugin, Server, Settings};
use bingo_sdk::{Env, ErrorCode, KernelError, LoginMethod, Plugin};
use clap::{Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

use crate::login::Terminal;

/// The plugin whose slice of the settings this reads and writes.
const PLUGIN: &str = "bingo.mcp";
const KEY: &str = "mcpServers";

/// What one `bingo mcp` line asks for. This is clap's own shape: the words a
/// person types and the values this acts on are one thing, so there is no
/// translation between them to keep in step.
#[derive(Clone, Debug, Subcommand)]
pub enum Action {
    /// Every configured server, merged across the settings layers.
    List,
    /// One server in full. Header and environment *names* only: their values
    /// are the person's secrets.
    Get {
        /// The server's name.
        name: String,
    },
    /// Write a server into the user settings layer, replacing one of the same
    /// name. Never a project file.
    Add {
        /// The name the model's tools will carry: `mcp__<name>__<tool>`.
        name: String,
        /// The url for `--transport http`, or the command and its arguments
        /// for a stdio server. Everything after the name belongs to it, so
        /// the options below go before the name.
        #[arg(
            required = true,
            num_args = 1..,
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        target: Vec<String>,
        #[arg(short = 't', long, value_enum, default_value = "stdio")]
        transport: Transport,
        /// An http header, `"Name: value"`. Repeatable.
        #[arg(short = 'H', long = "header", value_name = "HEADER")]
        headers: Vec<String>,
        /// An environment variable for a stdio server, `NAME=value`.
        /// Repeatable.
        #[arg(short = 'e', long = "env", value_name = "NAME=VALUE")]
        env: Vec<String>,
        /// The client id this server's authorization server gave you, when it
        /// registers no clients of its own.
        #[arg(long, value_name = "ID")]
        client_id: Option<String>,
    },
    /// Take a server out of the user settings layer.
    Remove {
        /// The server's name.
        name: String,
    },
    /// Sign in to a server that answers `401` (ADR-0050).
    Login {
        /// The server's name.
        name: String,
        /// Print the address instead of opening a browser, and read the
        /// redirect — or the code on the page — back from the keyboard.
        #[arg(long)]
        paste: bool,
    },
    /// Revoke and forget a server's stored sign-in.
    Logout {
        /// The server's name.
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Transport {
    /// A child process speaking the protocol over its stdin and stdout.
    Stdio,
    /// A streamable-HTTP endpoint.
    Http,
}

/// Do it, and answer with the one thing stdout carries.
pub async fn run(
    action: &Action,
    env: &Env,
    cwd: &Path,
    settings_path: Option<&Path>,
    extra: Option<Layer>,
) -> Result<i32, KernelError> {
    let said = match action {
        Action::Add { .. } | Action::Remove { .. } => written(action, env)?,
        _ => {
            let configured = configured(env, cwd, settings_path, extra)?;
            asked(action, env, &configured).await?
        }
    };
    println!("{said}");
    Ok(0)
}

/// The verbs that change the settings. They write the user layer and read
/// only it: a project's own servers are not this command's to rewrite.
fn written(action: &Action, env: &Env) -> Result<String, KernelError> {
    let path = settings::user_path(env);
    let mut servers = user_servers(&path)?;
    let said = match action {
        Action::Add {
            name,
            transport,
            target,
            headers,
            env: variables,
            client_id,
        } => {
            let entry = entry(*transport, target, headers, variables, client_id.as_deref())?;
            let replaced = servers.insert(name.clone(), entry).is_some();
            let verb = if replaced { "Replaced" } else { "Added" };
            format!("{verb} the mcp server `{name}` in {}.", path.display())
        }
        Action::Remove { name } => {
            if servers.remove(name).is_none() {
                return Err(invalid(format!(
                    "no mcp server named `{name}` in {}",
                    path.display()
                )));
            }
            format!("Removed the mcp server `{name}` from {}.", path.display())
        }
        _ => return Err(internal("this verb writes nothing")),
    };
    settings::remember(&path, &[(KEY, Value::Object(servers))])
        .map_err(|e| internal(e.to_string()))?;
    Ok(said)
}

/// The `mcpServers` object of the user layer, read to be written back. Only
/// this layer, because only this layer is written (ADR-0003 §5).
fn user_servers(path: &Path) -> Result<Map<String, Value>, KernelError> {
    let document = settings::read_document(path).map_err(|e| invalid(e.to_string()))?;
    match document.get(KEY) {
        None => Ok(Map::new()),
        Some(Value::Object(servers)) => Ok(servers.clone()),
        Some(_) => Err(invalid(format!(
            "`{KEY}` in {} is not an object",
            path.display()
        ))),
    }
}

/// One server as it is written on disk. Built here and parsed straight back,
/// so a line that would not have loaded is refused now rather than at the
/// next start.
fn entry(
    transport: Transport,
    target: &[String],
    headers: &[String],
    variables: &[String],
    client_id: Option<&str>,
) -> Result<Value, KernelError> {
    let Some((first, rest)) = target.split_first() else {
        return Err(invalid(
            "an mcp server needs a url (http) or a command (stdio)".to_string(),
        ));
    };
    let mut entry = match transport {
        Transport::Http => {
            if !rest.is_empty() {
                return Err(invalid(format!(
                    "an http server takes one url, not {}",
                    target.len()
                )));
            }
            if !variables.is_empty() {
                return Err(invalid(
                    "an environment variable belongs to a stdio server, \
                     which has a child process to give it to",
                ));
            }
            json!({ "type": "http", "url": first, "headers": pairs(headers, ':')? })
        }
        Transport::Stdio => {
            if !headers.is_empty() {
                return Err(invalid(
                    "a header belongs to an http server; a stdio server is a \
                     child process, and its secrets go in its environment",
                ));
            }
            json!({ "type": "stdio", "command": first, "args": rest, "env": pairs(variables, '=')? })
        }
    };
    if let Some(client_id) = client_id
        && let Some(object) = entry.as_object_mut()
    {
        object.insert("oauth".into(), json!({ "clientId": client_id }));
    }
    // The plugin owns what a row may say; parsing it here is how a mistake
    // becomes a refusal now instead of a server that never dials.
    serde_json::from_value::<Server>(entry.clone()).map_err(|e| invalid(e.to_string()))?;
    Ok(entry)
}

/// `-H "Name: value"` and `-e NAME=value`, in the shape the settings hold.
fn pairs(given: &[String], between: char) -> Result<Map<String, Value>, KernelError> {
    given
        .iter()
        .map(|pair| {
            let Some((name, value)) = pair.split_once(between) else {
                return Err(invalid(format!("`{pair}` is not `name{between}value`")));
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(invalid(format!("`{pair}` names nothing")));
            }
            Ok((name.to_string(), json!(value.trim())))
        })
        .collect()
}

/// The verbs that only read, and the two that sign in.
async fn asked(
    action: &Action,
    env: &Env,
    configured: &BTreeMap<String, Server>,
) -> Result<String, KernelError> {
    match action {
        Action::List => Ok(list(configured)),
        Action::Get { name } => Ok(describe(name, named(configured, name)?)),
        Action::Login { name, paste } => login(env, name, named(configured, name)?, *paste).await,
        Action::Logout { name } => logout(env, name, named(configured, name)?).await,
        _ => Err(internal("this verb reads nothing")),
    }
}

fn named<'a>(
    configured: &'a BTreeMap<String, Server>,
    name: &str,
) -> Result<&'a Server, KernelError> {
    configured.get(name).ok_or_else(|| {
        let names: Vec<&str> = configured.keys().map(String::as_str).collect();
        let known = match names.is_empty() {
            true => "no mcp servers are configured".to_string(),
            false => format!("configured: {}", names.join(", ")),
        };
        invalid(format!("no mcp server named `{name}` ({known})"))
    })
}

/// One line per server: its name, its transport and where it is.
fn list(configured: &BTreeMap<String, Server>) -> String {
    if configured.is_empty() {
        return "No mcp servers are configured.".to_string();
    }
    configured
        .iter()
        .map(|(name, server)| format!("{name}\t{}\t{}", transport_of(server), endpoint(server)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything about one server that is safe to print. A header value and an
/// environment value are secrets; their names are not.
fn describe(name: &str, server: &Server) -> String {
    let mut lines = vec![
        name.to_string(),
        format!("  type: {}", transport_of(server)),
    ];
    match server {
        Server::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            lines.push(format!("  command: {command}"));
            if !args.is_empty() {
                lines.push(format!("  args: {}", args.join(" ")));
            }
            if let Some(cwd) = cwd {
                lines.push(format!("  cwd: {}", cwd.display()));
            }
            lines.push(format!("  env: {}", names(env)));
        }
        Server::Http {
            url,
            headers,
            oauth,
        } => {
            lines.push(format!("  url: {url}"));
            lines.push(format!("  headers: {}", names(headers)));
            if let Some(client_id) = oauth.as_ref().and_then(|oauth| oauth.client_id.as_deref()) {
                lines.push(format!("  oauth client id: {client_id}"));
            }
        }
    }
    lines.join("\n")
}

/// The names a person gave, and never the values behind them.
fn names(map: &BTreeMap<String, String>) -> String {
    match map.is_empty() {
        true => "none".to_string(),
        false => map.keys().cloned().collect::<Vec<_>>().join(", "),
    }
}

fn transport_of(server: &Server) -> &'static str {
    match server {
        Server::Stdio { .. } => "stdio",
        Server::Http { .. } => "http",
    }
}

fn endpoint(server: &Server) -> String {
    match server {
        Server::Stdio { command, args, .. } => match args.is_empty() {
            true => command.clone(),
            false => format!("{command} {}", args.join(" ")),
        },
        Server::Http { url, .. } => url.clone(),
    }
}

/// Sign in from a terminal. The browser opens unless `--paste` was asked
/// for, in which case the address is printed and the redirect is typed back.
async fn login(env: &Env, name: &str, server: &Server, paste: bool) -> Result<String, KernelError> {
    let auth = signs_in(env, name, server)?;
    let method = paste.then_some(LoginMethod::Paste);
    auth.login(Arc::new(Terminal), method, !paste)
        .await
        .map_err(|e| invalid(e.to_string()))
}

async fn logout(env: &Env, name: &str, server: &Server) -> Result<String, KernelError> {
    signs_in(env, name, server)?
        .logout()
        .await
        .map_err(|e| internal(e.to_string()))
}

/// The sign-in handle, or the reason this server has none. The plugin owns
/// which servers take one, so this asks it rather than deciding again.
fn signs_in(
    env: &Env,
    name: &str,
    server: &Server,
) -> Result<Arc<bingo_auth_oauth::McpAuth>, KernelError> {
    let store = Arc::new(CredentialStore::new(env.data_dir.clone()));
    bingo_mcp::auth::handle(name, server, store, reqwest::Client::new()).ok_or_else(|| {
        invalid(format!(
            "`{name}` signs in to nothing: it is a child process, or its \
             Authorization header is already written in the settings"
        ))
    })
}

/// Every configured server, merged across the layers a run would read: what
/// `bingo mcp list` shows is what the next session dials.
fn configured(
    env: &Env,
    cwd: &Path,
    settings_path: Option<&Path>,
    extra: Option<Layer>,
) -> Result<BTreeMap<String, Server>, KernelError> {
    let mut layers = settings::load(env, cwd, settings_path).map_err(|e| invalid(e.to_string()))?;
    layers.extend(extra);
    let claim = Claim::from_manifest(McpPlugin::default().manifest())
        .ok_or_else(|| internal("the mcp plugin claims no settings"))?;
    let merged = settings::merge(&layers, &[claim]).map_err(|e| invalid(e.to_string()))?;
    let slice = merged.plugins.get(PLUGIN).cloned().unwrap_or(Value::Null);
    let settings: Settings = serde_json::from_value(slice).map_err(|e| invalid(e.to_string()))?;
    Ok(settings.mcp_servers)
}

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

fn internal(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str, args: &[&str]) -> Server {
        Server::Stdio {
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: BTreeMap::from([("GITHUB_TOKEN".to_string(), "ghp_live".to_string())]),
            cwd: None,
        }
    }

    fn http(url: &str) -> Server {
        Server::Http {
            url: url.into(),
            headers: BTreeMap::from([("X-Tenant".to_string(), "acme".to_string())]),
            oauth: None,
        }
    }

    #[test]
    fn an_http_entry_is_the_row_the_plugin_reads() {
        let written = entry(
            Transport::Http,
            &["https://mcp.example.com/mcp".to_string()],
            &["X-Tenant: acme".to_string()],
            &[],
            Some("cl_1"),
        )
        .expect("a row");
        assert_eq!(
            written,
            json!({
                "type": "http",
                "url": "https://mcp.example.com/mcp",
                "headers": { "X-Tenant": "acme" },
                "oauth": { "clientId": "cl_1" },
            })
        );
    }

    #[test]
    fn a_stdio_entry_keeps_the_arguments_after_the_command() {
        let written = entry(
            Transport::Stdio,
            &["npx".to_string(), "-y".to_string(), "files".to_string()],
            &[],
            &["TOKEN=s3cret".to_string()],
            None,
        )
        .expect("a row");
        assert_eq!(
            written,
            json!({
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "files"],
                "env": { "TOKEN": "s3cret" },
            })
        );
    }

    #[test]
    fn a_row_the_plugin_would_refuse_is_refused_here() {
        // A header on a child process is dropped nowhere: it is refused.
        let refused = entry(
            Transport::Stdio,
            &["npx".to_string()],
            &["X: y".to_string()],
            &[],
            None,
        )
        .expect_err("a header is an http server's");
        assert!(refused.message.contains("http server"), "{refused}");
        let refused = entry(
            Transport::Http,
            &["https://a".to_string()],
            &[],
            &["TOKEN=s3cret".to_string()],
            None,
        )
        .expect_err("an environment is a child process's");
        assert!(refused.message.contains("stdio server"), "{refused}");
        let refused = entry(Transport::Http, &[], &[], &[], None).expect_err("no url");
        assert!(refused.message.contains("needs a url"), "{refused}");
        let refused = entry(
            Transport::Http,
            &["https://a".to_string(), "https://b".to_string()],
            &[],
            &[],
            None,
        )
        .expect_err("two urls");
        assert!(refused.message.contains("one url"), "{refused}");
    }

    #[test]
    fn a_pair_that_is_not_a_pair_is_refused_by_the_text_that_was_typed() {
        assert_eq!(
            pairs(&["Authorization: Bearer t".to_string()], ':').expect("a header"),
            Map::from_iter([("Authorization".to_string(), json!("Bearer t"))])
        );
        let refused = pairs(&["nonsense".to_string()], ':').expect_err("no separator");
        assert!(refused.message.contains("`nonsense`"), "{refused}");
        let refused = pairs(&[": value".to_string()], ':').expect_err("no name");
        assert!(refused.message.contains("names nothing"), "{refused}");
    }

    /// ADR-0050 §4: `get` prints the *names* of a person's headers and
    /// environment, and never what is in them.
    #[test]
    fn get_prints_the_names_of_the_secrets_and_not_their_values() {
        let said = describe("files", &stdio("npx", &["-y", "files"]));
        assert!(said.contains("GITHUB_TOKEN"), "{said}");
        assert!(!said.contains("ghp_live"), "{said}");
        assert!(said.contains("command: npx"), "{said}");
        assert!(said.contains("args: -y files"), "{said}");

        let said = describe(
            "remote",
            &Server::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: BTreeMap::from([(
                    "Authorization".to_string(),
                    "Bearer ghp_live".to_string(),
                )]),
                oauth: None,
            },
        );
        assert!(said.contains("Authorization"), "{said}");
        assert!(!said.contains("ghp_live"), "{said}");
    }

    #[test]
    fn a_server_with_nothing_configured_says_none_rather_than_nothing() {
        let said = describe(
            "remote",
            &Server::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: BTreeMap::new(),
                oauth: None,
            },
        );
        assert!(said.contains("headers: none"), "{said}");
    }

    #[test]
    fn the_listing_is_one_line_per_server_and_says_so_when_there_are_none() {
        assert_eq!(list(&BTreeMap::new()), "No mcp servers are configured.");
        let configured = BTreeMap::from([
            ("files".to_string(), stdio("npx", &["-y", "files"])),
            ("remote".to_string(), http("https://mcp.example.com/mcp")),
        ]);
        assert_eq!(
            list(&configured),
            "files\tstdio\tnpx -y files\nremote\thttp\thttps://mcp.example.com/mcp"
        );
    }

    #[test]
    fn a_name_nobody_configured_is_refused_with_the_names_that_are() {
        let configured = BTreeMap::from([("files".to_string(), stdio("npx", &[]))]);
        let refused = named(&configured, "nope").expect_err("no such server");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(refused.message.contains("files"), "{refused}");
        let refused = named(&BTreeMap::new(), "nope").expect_err("none at all");
        assert!(refused.message.contains("no mcp servers"), "{refused}");
    }
}
