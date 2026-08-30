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

/// A name and its argument text, as the actor parses them (ADR-0008 §1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Parsed {
    pub name: String,
    pub args: String,
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
    if let Some(rest) = line.strip_prefix('!') {
        return Some(Parsed {
            name: "!".into(),
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

/// The origin a command's prompt is submitted with: the line's own, or none
/// for a typed action, which carries none on the wire.
pub(super) fn origin_of(input: &Input) -> Origin {
    match input {
        Input::Text { origin, .. } => origin.clone(),
        Input::Action { .. } => Origin::default(),
    }
}

/// One run the actor asks for.
pub(super) struct Run {
    pub intent: IntentId,
    pub origin: Origin,
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
    inflight: HashMap<IntentId, Origin>,
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
            origin,
            command,
            args,
            holds,
        } = run;
        self.inflight.insert(intent.clone(), origin);
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

    /// The run is over; its origin comes back for a `Prompt` outcome, with
    /// whether it was the run holding the queue.
    pub(super) fn finish(&mut self, intent: &IntentId) -> Option<(Origin, bool)> {
        let held = self.holding.as_ref() == Some(intent);
        if held {
            self.holding = None;
        }
        self.inflight.remove(intent).map(|origin| (origin, held))
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
