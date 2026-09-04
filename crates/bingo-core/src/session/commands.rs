//! Command dispatch (ADR-0008): a line is parsed here, its command looked up
//! in the one table, and run on its own task; the actor turns the outcome
//! into an ack. Nothing here awaits a command.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use bingo_sdk::*;
use futures::FutureExt;

use super::mailbox::{Mailbox, Msg};

/// What the session actor needs from the host to run commands.
pub struct Services {
    pub commands: Vec<Arc<dyn Command>>,
    /// Commands that arrive after I/O, consulted when the table has no such name.
    pub command_sources: Vec<Arc<dyn CommandSource>>,
    /// Weak: the host holds every session's mailbox, and a session must not
    /// hold the host.
    pub host: Weak<dyn HostApi>,
}

impl Services {
    /// No commands and no host: a session that only takes prose.
    pub fn none() -> Self {
        Self {
            commands: Vec::new(),
            command_sources: Vec::new(),
            host: Weak::<Unreachable>::new(),
        }
    }
}

/// The surface a command's own prompt carries (ADR-0008 §3). A `Prompt` is
/// the command speaking, not the keyboard: what re-enters the session is a
/// skill's body, and the surface is how every client tells the two apart.
pub(super) const SURFACE: &str = "command";

/// The one name that is its own prefix.
const BANG: &str = "!";

/// Whether a name is the shell line's (ADR-0008 §5).
pub(super) fn is_shell(name: &str) -> bool {
    name == BANG
}

/// Whether this submitter may run a shell line. A `!` is not a message to
/// anybody: it runs a program on the machine bingo runs on, with the
/// privileges bingo was started with, and nothing asks first. Only the person
/// the session works for may write one — the same bareness the provider fold
/// reads a person's own line by (`crate::context`). An agent's post, a room's
/// nudge, a correspondent writing from a chat all sign what they send, and a
/// signed line that starts with `!` is refused rather than run. A typed
/// action carries no origin at all, which is the same answer.
pub(super) fn may_run_shell(input: &Input) -> bool {
    match input {
        Input::Text { origin, .. } => crate::context::the_persons_own(origin),
        Input::Action { .. } => false,
    }
}

/// What a name nothing answers to is refused with. A `!` that found nothing
/// is not a misspelling: the shell a bang line runs in is a plugin's, and a
/// host that registered none has no shell at all rather than a typo.
pub(super) fn unknown(name: &str) -> String {
    match name {
        BANG => format!("no shell here: nothing answers to {}", spelled(BANG)),
        name => format!("unknown command: {}", spelled(name)),
    }
}

/// A name and its argument text, as the actor parses them (ADR-0008 §1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Parsed {
    pub name: String,
    pub args: String,
}

impl Parsed {
    /// The line as it would have been typed: what a person recognises the run
    /// by, and — for an `Input::Action`, which was never typed at all — what
    /// they would have typed for it.
    pub(super) fn typed(&self) -> String {
        let name = spelled(&self.name);
        match (self.args.is_empty(), self.name.as_str()) {
            (true, _) => name,
            (false, BANG) => format!("{name}{}", self.args),
            (false, _) => format!("{name} {}", self.args),
        }
    }
}

/// How a command's name is written: `!` is its own prefix, everything else
/// follows a slash.
pub(super) fn spelled(name: &str) -> String {
    match name {
        BANG => BANG.to_string(),
        name => format!("/{name}"),
    }
}

/// `/name rest` → `name`, `!rest` → `!`, an action → its name; prose → `None`.
pub(super) fn parse(input: &Input) -> Option<Parsed> {
    match input {
        Input::Text { text, .. } => parse_line(text),
        Input::Action { action } => Some(Parsed {
            name: action.name.clone(),
            args: match &action.args {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            },
        }),
    }
}

fn parse_line(text: &str) -> Option<Parsed> {
    let line = text.trim_start();
    if let Some(rest) = line.strip_prefix(BANG) {
        return Some(Parsed {
            name: BANG.into(),
            args: rest.trim().to_string(),
        });
    }
    let rest = line.strip_prefix('/')?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest.trim_end(), ""),
    };
    if name.is_empty() {
        return None;
    }
    Some(Parsed {
        name: name.to_string(),
        args: args.to_string(),
    })
}

pub(super) fn is_command(input: &Input) -> bool {
    parse(input).is_some()
}

/// How a command was invoked, kept for as long as it runs: the line that
/// asked for it, and the origin whatever it produces re-enters the session
/// with.
#[derive(Clone, Debug)]
pub(super) struct Invocation {
    pub line: String,
    pub origin: Origin,
}

impl Invocation {
    /// What the submitter's origin becomes once a command stands between them
    /// and the session: the same person, in the same conversation, through the
    /// command surface (ADR-0008 §3). A typed action carries no origin on the
    /// wire, so it starts from none.
    pub(super) fn of(input: &Input, parsed: &Parsed) -> Self {
        let submitted = match input {
            Input::Text { origin, .. } => origin.clone(),
            Input::Action { .. } => Origin::default(),
        };
        Self {
            line: parsed.typed(),
            origin: Origin {
                surface: SURFACE.into(),
                ..submitted
            },
        }
    }

    /// A command's prompt as the journal keeps it: the line that asked for it,
    /// then what it produced. The line is written down once, here — a client
    /// reads the run back off the item rather than off a field beside it.
    pub(super) fn prompt(&self, text: &str) -> String {
        format!("{}\n\n{text}", self.line)
    }
}

/// One run the actor asks for.
pub(super) struct Run {
    pub intent: IntentId,
    pub invocation: Invocation,
    pub command: Arc<dyn Command>,
    pub args: String,
    /// A non-instant run, which the queue waits behind.
    pub holds: bool,
}

/// The table and the runs in flight.
pub(super) struct Commands {
    table: Vec<Arc<dyn Command>>,
    sources: Vec<Arc<dyn CommandSource>>,
    host: Weak<dyn HostApi>,
    mailbox: Mailbox,
    session: SessionId,
    cwd: PathBuf,
    inflight: HashMap<IntentId, Invocation>,
    /// The non-instant command whose run holds the queue behind it.
    holding: Option<IntentId>,
}

impl Commands {
    pub(super) fn new(services: Services, mailbox: Mailbox, cwd: PathBuf) -> Self {
        Self {
            table: services.commands,
            sources: services.command_sources,
            host: services.host,
            session: mailbox.id().clone(),
            mailbox,
            cwd,
            inflight: HashMap::new(),
            holding: None,
        }
    }

    /// The table first, then each source in order (ADR-0009).
    pub(super) async fn find(&self, name: &str) -> Option<Arc<dyn Command>> {
        if let Some(found) = named(&self.table, name) {
            return Some(found);
        }
        for source in &self.sources {
            if let Some(found) = named(&source.commands(&self.cwd).await, name) {
                return Some(found);
            }
        }
        None
    }

    /// A non-instant command is running: the queue waits.
    pub(super) fn busy(&self) -> bool {
        self.holding.is_some()
    }

    /// Run on a task that reports back by mail.
    pub(super) fn spawn(&mut self, run: Run) -> Result<(), KernelError> {
        let host = self
            .host
            .upgrade()
            .ok_or_else(|| KernelError::new(ErrorCode::SessionClosed, "the host is gone"))?;
        let cx = CommandContext {
            session: self.session.clone(),
            cwd: self.cwd.clone(),
            host: HostHandle(host),
        };
        let Run {
            intent,
            invocation,
            command,
            args,
            holds,
        } = run;
        self.inflight.insert(intent.clone(), invocation);
        if holds {
            self.holding = Some(intent.clone());
        }
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let outcome = AssertUnwindSafe(command.run(&args, &cx))
                .catch_unwind()
                .await
                .unwrap_or_else(|panic| {
                    Err(KernelError::new(
                        ErrorCode::Internal,
                        format!("the command panicked: {}", super::panic_message(panic)),
                    ))
                });
            mailbox.send(Msg::CommandFinished { intent, outcome });
        });
        Ok(())
    }

    /// The run is over; how it was invoked comes back for a `Prompt` outcome,
    /// with whether it was the run holding the queue.
    pub(super) fn finish(&mut self, intent: &IntentId) -> Option<(Invocation, bool)> {
        let held = self.holding.as_ref() == Some(intent);
        if held {
            self.holding = None;
        }
        self.inflight
            .remove(intent)
            .map(|invocation| (invocation, held))
    }
}

fn named(table: &[Arc<dyn Command>], name: &str) -> Option<Arc<dyn Command>> {
    table
        .iter()
        .find(|c| {
            let spec = c.spec();
            spec.name == name || spec.aliases.iter().any(|a| a == name)
        })
        .cloned()
}

/// A host that is never there, for `Services::none`.
struct Unreachable;

#[async_trait::async_trait]
impl HostApi for Unreachable {
    async fn sessions(&self, _: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        unreachable!("never constructed")
    }
    async fn open(
        &self,
        _: SessionSelector,
        _: ClientIdentity,
        _: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        unreachable!("never constructed")
    }
    async fn close(&self, _: &SessionId, _: CloseReason) -> Result<(), KernelError> {
        unreachable!("never constructed")
    }
    async fn delete(&self, _: &SessionId) -> Result<(), KernelError> {
        unreachable!("never constructed")
    }

    async fn deliver(
        &self,
        _to: &SessionId,
        _intent: IntentId,
        _input: Input,
        _delivery: Delivery,
    ) -> Result<(), KernelError> {
        unreachable!("never constructed")
    }

    async fn extend(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("never constructed")
    }

    async fn signal(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("never constructed")
    }
    async fn catalog(&self, _: CatalogKind) -> Result<Catalog, KernelError> {
        unreachable!("never constructed")
    }
    fn gateway_events(&self) -> GatewayStream {
        unreachable!("never constructed")
    }
    fn service_any(&self, _: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        unreachable!("never constructed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(t: &str) -> Input {
        Input::text(t, Origin::surface("test"))
    }

    #[test]
    fn a_slash_line_is_a_name_and_its_arguments() {
        assert_eq!(
            parse(&text("/model  anthropic/x ")),
            Some(Parsed {
                name: "model".into(),
                args: "anthropic/x".into()
            })
        );
        assert_eq!(
            parse(&text("  /help")),
            Some(Parsed {
                name: "help".into(),
                args: String::new()
            })
        );
        assert_eq!(parse(&text("/")), None, "a bare slash is prose");
        assert_eq!(parse(&text("hello /world")), None);
    }

    #[test]
    fn a_bang_line_is_the_shell_command_with_the_rest_verbatim() {
        assert_eq!(
            parse(&text("!ls -la | head")),
            Some(Parsed {
                name: "!".into(),
                args: "ls -la | head".into()
            })
        );
    }

    /// What a run is recognised by, whatever door it came through.
    #[test]
    fn a_run_is_labelled_by_the_line_that_would_have_typed_it() {
        let typed = |line: &str| parse(&text(line)).map(|p| p.typed());
        assert_eq!(
            typed("/model  anthropic/x "),
            Some("/model anthropic/x".into())
        );
        assert_eq!(typed("  /help"), Some("/help".into()));
        assert_eq!(typed("!ls -la | head"), Some("!ls -la | head".into()));
        assert_eq!(typed("!"), Some("!".into()));
        let action = Input::Action {
            action: Action {
                name: "permission".into(),
                args: json!("plan"),
            },
        };
        assert_eq!(
            parse(&action).map(|p| p.typed()),
            Some("/permission plan".into()),
            "a button nobody typed still reads as the line it stands for"
        );
    }

    /// The submitter stays who they are; only the surface says a command now
    /// stands between them and the session.
    #[test]
    fn an_invocation_keeps_the_submitter_and_takes_the_command_surface() {
        let input = Input::text(
            "/guide do the thing",
            Origin {
                surface: "channels".into(),
                principal: Some("ou_person".into()),
                conversation: Some("loopback/oc_1".into()),
            },
        );
        let parsed = parse(&input).expect("a command line");
        let invocation = Invocation::of(&input, &parsed);
        assert_eq!(invocation.line, "/guide do the thing");
        assert_eq!(
            invocation.origin,
            Origin {
                surface: SURFACE.into(),
                principal: Some("ou_person".into()),
                conversation: Some("loopback/oc_1".into()),
            }
        );
        assert_eq!(
            invocation.prompt("Read the guide."),
            "/guide do the thing\n\nRead the guide.",
            "the typed line is the item's first line and its only record"
        );
    }

    /// The bareness the fold reads a person's own line by is the same bareness
    /// a shell line is allowed by: a door that signs what it sends may not
    /// run one (M65).
    #[test]
    fn only_the_persons_own_line_may_run_a_shell_command() {
        let from = |origin: Origin| {
            may_run_shell(&Input::Text {
                text: "!ls".into(),
                images: Vec::new(),
                origin,
                delivery: Delivery::Wake,
            })
        };
        assert!(from(Origin::surface("tui")));
        assert!(from(Origin::surface("print")));
        assert!(!from(Origin {
            surface: "agent".into(),
            principal: Some("scout".into()),
            conversation: None,
        }));
        assert!(!from(Origin {
            surface: "channels".into(),
            principal: Some("ou_person".into()),
            conversation: Some("loopback/oc_1".into()),
        }));
        assert!(
            !from(Origin {
                surface: "peer".into(),
                principal: None,
                conversation: Some("#design".into()),
            }),
            "a post signs where it came from"
        );
        assert!(
            !from(Origin::surface("kernel")),
            "the kernel's own voice is not the person's"
        );
        assert!(
            !may_run_shell(&Input::Action {
                action: Action {
                    name: BANG.into(),
                    args: json!("ls"),
                },
            }),
            "a button carries no origin, and a shell line needs one"
        );
    }

    /// A `!` nothing answers to is not a misspelling.
    #[test]
    fn a_shell_line_with_no_shell_says_that_is_what_is_missing() {
        assert!(is_shell(BANG) && !is_shell("model"));
        assert!(unknown(BANG).contains("shell"), "{}", unknown(BANG));
        assert_eq!(unknown("nope"), "unknown command: /nope");
    }

    #[test]
    fn an_action_is_its_name_and_its_arguments_as_text_or_json() {
        let action = |args| Input::Action {
            action: Action {
                name: "permission".into(),
                args,
            },
        };
        assert_eq!(
            parse(&action(json!("plan"))).map(|p| p.args),
            Some("plan".into())
        );
        assert_eq!(
            parse(&action(json!(null))).map(|p| p.args),
            Some(String::new())
        );
        assert_eq!(
            parse(&action(json!({"mode": "plan"}))).map(|p| p.args),
            Some(r#"{"mode":"plan"}"#.into())
        );
    }
}
