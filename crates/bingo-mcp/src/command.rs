//! `/mcp`: what every configured server is doing, and the three verbs that
//! change it.
//!
//! A verb answers the moment it has started something, not when it has
//! finished: a handshake takes seconds, and a command that waited for one
//! would be a command that hangs. What it did shows up in the next `/mcp`.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, KernelError, View,
};

use crate::manager::{Manager, Status};

pub struct McpCommand {
    manager: Arc<Manager>,
}

impl McpCommand {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }

    async fn table(&self) -> CommandOutcome {
        CommandOutcome::View {
            view: View::Table {
                headers: vec!["server".into(), "status".into(), "tools".into()],
                rows: self.manager.statuses().await.iter().map(row).collect(),
            },
        }
    }

    async fn act(&self, verb: Verb, server: &str) -> Result<CommandOutcome, KernelError> {
        if !self.manager.knows(server) {
            return Err(unknown_server(&self.manager, server));
        }
        let message = match verb {
            Verb::Reconnect => self.reconnect(server).await,
            Verb::Enable => self.enable(server).await,
            Verb::Disable => self.disable(server).await,
        };
        Ok(CommandOutcome::Applied {
            message: Some(message),
        })
    }

    async fn reconnect(&self, server: &str) -> String {
        if self.manager.reconnect(server).await {
            format!("dialling {server} again")
        } else {
            format!("{server} is disabled; /mcp enable {server} first")
        }
    }

    async fn enable(&self, server: &str) -> String {
        if self.manager.enable(server).await {
            format!("enabled {server}; dialling it")
        } else {
            format!("{server} is already enabled")
        }
    }

    async fn disable(&self, server: &str) -> String {
        if self.manager.disable(server).await {
            format!("disabled {server}")
        } else {
            format!("{server} is already disabled")
        }
    }
}

/// One server's line: what it is doing, and how many tools it gave us.
fn row((name, status): &(String, Status)) -> Vec<String> {
    let (state, tools) = match status {
        Status::Connecting => ("connecting".to_string(), String::new()),
        Status::Connected { tools } => ("connected".to_string(), tools.to_string()),
        Status::Failed { why } => (format!("failed: {why}"), String::new()),
        Status::Disabled => ("disabled".to_string(), String::new()),
    };
    vec![name.clone(), state, tools]
}

fn unknown_server(manager: &Manager, server: &str) -> KernelError {
    let configured: Vec<&str> = manager.names().collect();
    let known = if configured.is_empty() {
        "no mcp servers are configured".to_string()
    } else {
        format!("configured: {}", configured.join(", "))
    };
    KernelError::new(
        ErrorCode::InvalidInput,
        format!("no mcp server named {server} ({known})"),
    )
}

/// What a `/mcp` line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    List,
    Act { verb: Verb, server: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Reconnect,
    Enable,
    Disable,
}

impl Verb {
    const ALL: [Verb; 3] = [Verb::Reconnect, Verb::Enable, Verb::Disable];

    fn as_str(self) -> &'static str {
        match self {
            Verb::Reconnect => "reconnect",
            Verb::Enable => "enable",
            Verb::Disable => "disable",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Verb::ALL.into_iter().find(|verb| verb.as_str() == word)
    }
}

impl Request {
    pub fn parse(args: &str) -> Result<Self, KernelError> {
        let mut words = args.split_whitespace();
        let Some(word) = words.next() else {
            return Ok(Request::List);
        };
        let verb =
            Verb::parse(word).ok_or_else(|| invalid(format!("{word} is not a /mcp verb")))?;
        let Some(server) = words.next() else {
            return Err(invalid(format!("{word} names no server")));
        };
        if words.next().is_some() {
            return Err(invalid(format!("{word} takes one server")));
        }
        Ok(Request::Act {
            verb,
            server: server.to_string(),
        })
    }
}

fn invalid(what: String) -> KernelError {
    let verbs = Verb::ALL.map(Verb::as_str).join(" | ");
    KernelError::new(
        ErrorCode::InvalidInput,
        format!("{what}; /mcp [{verbs} <server>]"),
    )
}

#[async_trait]
impl Command for McpCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "mcp".into(),
            aliases: Vec::new(),
            hint: "[reconnect|enable|disable <server>]".into(),
            args: ArgSpec::Free {
                hint: "reconnect <server> | enable <server> | disable <server>".into(),
            },
            // Reading the table and starting a dial touch nothing a turn is
            // using; the tool set a turn already gathered stays as it was.
            instant: true,
            family: "mcp".into(),
        }
    }

    async fn run(&self, args: &str, _cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        match Request::parse(args)? {
            Request::List => Ok(self.table().await),
            Request::Act { verb, server } => self.act(verb, &server).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(verb: Verb, server: &str) -> Request {
        Request::Act {
            verb,
            server: server.to_string(),
        }
    }

    fn parse(args: &str) -> Result<Request, KernelError> {
        Request::parse(args)
    }

    #[test]
    fn nothing_asks_for_the_table() {
        assert_eq!(parse("").expect("a listing"), Request::List);
        assert_eq!(parse("   ").expect("a listing"), Request::List);
    }

    #[test]
    fn a_verb_and_a_server_are_an_action() {
        assert_eq!(
            parse("reconnect files").expect("an action"),
            act(Verb::Reconnect, "files")
        );
        assert_eq!(
            parse("  enable   files  ").expect("an action"),
            act(Verb::Enable, "files")
        );
        assert_eq!(
            parse("disable files").expect("an action"),
            act(Verb::Disable, "files")
        );
    }

    #[test]
    fn a_verb_nobody_defined_is_refused_with_the_three_that_exist() {
        let error = parse("restart files").expect_err("not a verb");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        for verb in Verb::ALL {
            assert!(error.message.contains(verb.as_str()), "{error}");
        }
    }

    #[test]
    fn a_verb_without_a_server_is_refused() {
        let error = parse("reconnect").expect_err("no server");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("names no server"), "{error}");
    }

    #[test]
    fn a_verb_with_two_servers_is_refused() {
        let error = parse("disable files web").expect_err("two servers");
        assert!(error.message.contains("takes one server"), "{error}");
    }

    #[test]
    fn a_row_says_what_a_server_is_doing_and_how_much_it_gave() {
        assert_eq!(
            row(&("files".into(), Status::Connected { tools: 3 })),
            ["files", "connected", "3"]
        );
        assert_eq!(
            row(&("files".into(), Status::Connecting)),
            ["files", "connecting", ""]
        );
        assert_eq!(
            row(&("files".into(), Status::Disabled)),
            ["files", "disabled", ""]
        );
        assert_eq!(
            row(&(
                "files".into(),
                Status::Failed {
                    why: "connect timed out after 5s".into()
                }
            )),
            ["files", "failed: connect timed out after 5s", ""]
        );
    }

    #[test]
    fn the_spec_runs_now_and_takes_a_verb_and_a_server() {
        let manager = Arc::new(Manager::new(
            Default::default(),
            &[],
            std::env::temp_dir().join("bingo-mcp-command-tests"),
        ));
        let spec = McpCommand::new(manager).spec();
        assert_eq!(spec.name, "mcp");
        assert!(spec.instant, "reading a table never waits for a turn");
        assert_eq!(spec.family, "mcp");
        let ArgSpec::Free { hint } = spec.args else {
            panic!("a verb and a server are free text");
        };
        for verb in Verb::ALL {
            assert!(hint.contains(verb.as_str()), "{verb:?} is not in the hint");
        }
    }
}
