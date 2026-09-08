//! `/mcp`: what every configured server is doing, and the verbs that change
//! it.
//!
//! A verb that only starts something answers the moment it has started it: a
//! handshake takes seconds, and a command that waited for one would be a
//! command that hangs. `login` is the exception and the reason this command
//! is not instant — it takes minutes and asks through the session's own
//! dialog, so the queue waits behind it, as `/login` does (ADR-0012 §5).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode,
    HostHandle, InteractionKind, KernelError, Prompter, SessionId, View,
};

use crate::manager::{Line, Manager, Status};

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
                headers: vec![
                    "server".into(),
                    "status".into(),
                    "tools".into(),
                    "auth".into(),
                ],
                rows: self.manager.lines().await.iter().map(row).collect(),
            },
        }
    }

    /// What one connected server offers, by name.
    async fn tools(&self, server: &str) -> Result<CommandOutcome, KernelError> {
        let Some(tools) = self.manager.tools_of(server).await else {
            return Ok(CommandOutcome::Applied {
                message: Some(format!(
                    "{server} is not connected; /mcp reconnect {server}"
                )),
            });
        };
        Ok(CommandOutcome::View {
            view: View::Table {
                headers: vec!["tool".into(), "description".into()],
                rows: tools
                    .into_iter()
                    .map(|(name, described)| vec![name, described])
                    .collect(),
            },
        })
    }

    async fn act(
        &self,
        verb: Verb,
        server: &str,
        cx: &CommandContext,
    ) -> Result<CommandOutcome, KernelError> {
        if !self.manager.knows(server) {
            return Err(unknown_server(&self.manager, server));
        }
        let message = match verb {
            Verb::Tools => return self.tools(server).await,
            Verb::Login => self.login(server, cx).await?,
            Verb::Logout => self.logout(server).await?,
            Verb::Reconnect => self.reconnect(server).await,
            Verb::Enable => self.enable(server).await,
            Verb::Disable => self.disable(server).await,
        };
        Ok(CommandOutcome::Applied {
            message: Some(message),
        })
    }

    /// Authenticate, and re-authenticate: the same flow either way, through
    /// the session's own dialog. A server that lands signed in is dialled
    /// again at once, because its tools are what the sign-in was for.
    async fn login(&self, server: &str, cx: &CommandContext) -> Result<String, KernelError> {
        let auth = self.signs_in(server)?;
        let prompter = Arc::new(SessionPrompter {
            host: cx.host.clone(),
            session: cx.session.clone(),
        });
        let receipt = auth
            .login(prompter, None, true)
            .await
            .map_err(|e| unanswered(server, e))?;
        self.manager.reconnect(server).await;
        Ok(format!("{receipt} Dialling {server} again."))
    }

    /// Clear the sign-in: revoked where the issuer offers it, removed here,
    /// and the server dialled again so the table says what it now is.
    async fn logout(&self, server: &str) -> Result<String, KernelError> {
        let auth = self.signs_in(server)?;
        let receipt = auth
            .logout()
            .await
            .map_err(|e| KernelError::new(ErrorCode::Internal, e.to_string()))?;
        self.manager.reconnect(server).await;
        Ok(receipt)
    }

    fn signs_in(&self, server: &str) -> Result<Arc<bingo_auth_oauth::McpAuth>, KernelError> {
        self.manager.auth(server).cloned().ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                format!(
                    "{server} signs in to nothing: it is a child process, or its \
                     Authorization header is already written in the settings"
                ),
            )
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

/// A sign-in nobody finished. A person's `esc` and a surface that renders no
/// `Login` — the print one declines the question rather than showing it —
/// arrive as the same cancel, and the word alone would leave the second
/// reader not knowing what to do next; the headless twin is what they do
/// next (ADR-0050 §4).
fn unanswered(server: &str, error: bingo_auth_oauth::AuthError) -> KernelError {
    let message = match error {
        bingo_auth_oauth::AuthError::Cancelled => format!(
            "the sign-in to {server} was cancelled; \
             from a terminal, `bingo mcp login {server}` signs in without a dialog"
        ),
        other => other.to_string(),
    };
    KernelError::new(ErrorCode::InvalidInput, message)
}

/// The session's own way of asking a person, for a command that holds the
/// queue while a sign-in runs. The kernel's door takes the session and the
/// question; this is the shape the library's flows want it in.
struct SessionPrompter {
    host: HostHandle,
    session: SessionId,
}

#[async_trait]
impl Prompter for SessionPrompter {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.host.ask(&self.session, kind, answers).await
    }
}

/// One server's line: what it is doing, how many tools it gave us, and where
/// its sign-in stands.
fn row(line: &Line) -> Vec<String> {
    let (state, tools) = match &line.status {
        Status::Connecting => ("connecting".to_string(), String::new()),
        Status::Connected { tools } => ("connected".to_string(), tools.to_string()),
        Status::NeedsAuth { .. } => (NEEDS_AUTH.to_string(), String::new()),
        Status::Failed { why } => (format!("failed: {why}"), String::new()),
        Status::Disabled => ("disabled".to_string(), String::new()),
    };
    vec![
        line.server.clone(),
        state,
        tools,
        signin(line.auth.as_ref()),
    ]
}

const NEEDS_AUTH: &str = "needs authentication";

/// A dash where there is no sign-in to speak of: a server that signs in to
/// nothing, or one nobody has signed in to — whether it wants a sign-in is
/// the status column's word, since a server that never asked for one has
/// nothing missing.
fn signin(auth: Option<&bingo_auth_oauth::Status>) -> String {
    use bingo_auth_oauth::Status;
    match auth {
        None | Some(Status::SignedOut) => "-".to_string(),
        Some(Status::SignedIn { .. }) => "signed in".to_string(),
        Some(Status::Expired { .. }) => "expired".to_string(),
    }
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
    Login,
    Logout,
    Tools,
    Reconnect,
    Enable,
    Disable,
}

impl Verb {
    const ALL: [Verb; 6] = [
        Verb::Login,
        Verb::Logout,
        Verb::Tools,
        Verb::Reconnect,
        Verb::Enable,
        Verb::Disable,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Verb::Login => "login",
            Verb::Logout => "logout",
            Verb::Tools => "tools",
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

const HINT: &str = "[login|logout|tools|reconnect|enable|disable <server>]";

#[async_trait]
impl Command for McpCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "mcp".into(),
            aliases: Vec::new(),
            hint: HINT.into(),
            args: ArgSpec::Free {
                hint: "login <server> | logout <server> | tools <server> | \
                       reconnect <server> | enable <server> | disable <server>"
                    .into(),
            },
            // A sign-in takes minutes and asks through the session's dialog,
            // so this command holds the queue as `/login` does (ADR-0012 §5).
            instant: false,
            family: "mcp".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        match Request::parse(args)? {
            Request::List => Ok(self.table().await),
            Request::Act { verb, server } => self.act(verb, &server, cx).await,
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

    fn line(status: Status, auth: Option<bingo_auth_oauth::Status>) -> Line {
        Line {
            server: "files".into(),
            status,
            auth,
        }
    }

    #[test]
    fn nothing_asks_for_the_table() {
        assert_eq!(parse("").expect("a listing"), Request::List);
        assert_eq!(parse("   ").expect("a listing"), Request::List);
    }

    #[test]
    fn every_verb_takes_one_server() {
        assert_eq!(
            parse("reconnect files").expect("an action"),
            act(Verb::Reconnect, "files")
        );
        assert_eq!(
            parse("  enable   files  ").expect("an action"),
            act(Verb::Enable, "files")
        );
        assert_eq!(
            parse("login remote").expect("an action"),
            act(Verb::Login, "remote")
        );
        assert_eq!(
            parse("logout remote").expect("an action"),
            act(Verb::Logout, "remote")
        );
        assert_eq!(
            parse("tools remote").expect("an action"),
            act(Verb::Tools, "remote")
        );
    }

    #[test]
    fn a_verb_nobody_defined_is_refused_with_the_ones_that_exist() {
        let error = parse("restart files").expect_err("not a verb");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        for verb in Verb::ALL {
            assert!(error.message.contains(verb.as_str()), "{error}");
        }
    }

    #[test]
    fn a_verb_without_a_server_or_with_two_is_refused() {
        let error = parse("login").expect_err("no server");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("names no server"), "{error}");
        let error = parse("disable files web").expect_err("two servers");
        assert!(error.message.contains("takes one server"), "{error}");
    }

    #[test]
    fn a_row_says_what_a_server_is_doing_and_how_much_it_gave() {
        assert_eq!(
            row(&line(Status::Connected { tools: 3 }, None)),
            ["files", "connected", "3", "-"]
        );
        assert_eq!(
            row(&line(Status::Connecting, None)),
            ["files", "connecting", "", "-"]
        );
        assert_eq!(
            row(&line(Status::Disabled, None)),
            ["files", "disabled", "", "-"]
        );
        assert_eq!(
            row(&line(
                Status::Failed {
                    why: "connect timed out after 5s".into()
                },
                None
            )),
            ["files", "failed: connect timed out after 5s", "", "-"]
        );
    }

    /// ADR-0050 §3: the two columns are two facts. A server can want a
    /// sign-in and be signed out, or be connected on a credential that is
    /// about to be renewed; one that never asked for a sign-in is missing
    /// nothing, so a signed-out server reads as a dash either way.
    #[test]
    fn the_auth_column_says_where_the_sign_in_stands() {
        use bingo_auth_oauth::Status as Signin;
        assert_eq!(
            row(&line(
                Status::NeedsAuth {
                    why: "handshake: 401".into()
                },
                Some(Signin::SignedOut)
            )),
            ["files", "needs authentication", "", "-"]
        );
        assert_eq!(
            row(&line(
                Status::Connected { tools: 3 },
                Some(Signin::SignedOut)
            )),
            ["files", "connected", "3", "-"]
        );
        assert_eq!(
            row(&line(
                Status::Connected { tools: 2 },
                Some(Signin::SignedIn { account: None })
            )),
            ["files", "connected", "2", "signed in"]
        );
        assert_eq!(
            row(&line(
                Status::NeedsAuth {
                    why: "handshake: 401".into()
                },
                Some(Signin::Expired {
                    reason: "refresh_token_expired".into()
                })
            )),
            ["files", "needs authentication", "", "expired"]
        );
    }

    #[test]
    fn the_spec_holds_the_queue_and_names_every_verb() {
        let manager = Arc::new(Manager::new(
            Default::default(),
            &[],
            std::env::temp_dir().join("bingo-mcp-command-tests"),
        ));
        let spec = McpCommand::new(manager).spec();
        assert_eq!(spec.name, "mcp");
        assert!(!spec.instant, "a sign-in asks a person and takes minutes");
        assert_eq!(spec.family, "mcp");
        let ArgSpec::Free { hint } = spec.args else {
            panic!("a verb and a server are free text");
        };
        for verb in Verb::ALL {
            assert!(hint.contains(verb.as_str()), "{verb:?} is not in the hint");
            assert!(spec.hint.contains(verb.as_str()), "{verb:?} is not offered");
        }
    }

    /// A surface that cannot show a sign-in declines it, and *cancelled* is
    /// not a thing a person can act on: the refusal names the way through.
    #[test]
    fn a_sign_in_nobody_answered_names_the_headless_way_through() {
        let refused = unanswered("remote", bingo_auth_oauth::AuthError::Cancelled);
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(
            refused.message.contains("bingo mcp login remote"),
            "{refused}"
        );
        let other = unanswered(
            "remote",
            bingo_auth_oauth::AuthError::Invalid("no S256".into()),
        );
        assert!(other.message.contains("no S256"), "{other}");
    }

    /// A server with no sign-in of its own is told so by name rather than
    /// being sent through a flow that has nowhere to go.
    #[tokio::test]
    async fn a_verb_that_signs_in_refuses_a_server_that_signs_in_to_nothing() {
        let servers = std::collections::BTreeMap::from([(
            "files".to_string(),
            crate::config::Server::Stdio {
                command: "/bin/echo".into(),
                args: Vec::new(),
                env: Default::default(),
                cwd: None,
            },
        )]);
        let manager = Arc::new(Manager::new(
            servers,
            &[],
            std::env::temp_dir().join("bingo-mcp-command-signin-tests"),
        ));
        let refused = McpCommand::new(manager)
            .signs_in("files")
            .expect_err("a child process signs in to nothing");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(refused.message.contains("child process"), "{refused}");
    }
}
