//! One table for every command bingo has.
//!
//! There used to be five. The terminal's `COMMANDS` fed `/help` and the
//! dropdown; `INSTANT_COMMANDS` decided which lines skip a running turn; a
//! `match` in `Chat` dispatched them; a second `match` in `complete` offered
//! their arguments; and a test copied the dispatch arms back out to check the
//! first against the third. Five hand-kept copies of one fact, and they had
//! drifted:
//!
//! - `/?` and `/quit` dispatched but appeared in no table, so `/help` skipped
//!   the queue while `/?` waited behind a turn, and neither was ever listed;
//! - `/team`'s hint advertised five subcommands out of nine;
//! - `/provider login` and `/provider logout` existed only as a hand-appended
//!   line in `/help`;
//! - the argument dropdown knew five commands out of twenty-four, and `/join`'s
//!   own error message promised a room typeahead that had been removed.
//!
//! This module is that one fact. A command has a name, the names it also answers
//! to, an argument grammar, whether it waits behind a running turn, and a parser
//! that says what the line *is*: a mutation the core performs ([`Action`]), a
//! lifecycle call, or a read whose view each frontend renders for itself.
//! `action/list` publishes the action half, the composer parses with the command
//! half, `/help` and the menu are drawn from it, and the completeness tests at
//! the bottom refuse to let the two drift apart again.
//!
//! Spec: `notes/design/gui-app-server.md` "Typed actions".

use std::path::PathBuf;

use crate::app::command::{
    Action, ActionArgument, ActionFamily, ActionId, ActionInfo, ArgumentKind, RewindTarget,
};
use crate::app::snapshot::{
    CatalogKind, ConfigSection, PermissionMode, PermissionRuleDecision, ResourceKind,
    RevisionScope, RewindMode, SessionLocator, ThemeChoice, ThinkingLevel,
};

// ---------------------------------------------------------------------------
// What a command turns into
// ---------------------------------------------------------------------------

/// What one composed command line means.
///
/// The three arms are the three shapes a CLI command has always had, named at
/// last: it changes session state, it changes which session there is, or it
/// shows something. Only the first is an [`Action`] — a view has no wire form
/// because its text is a projection, not a contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A mutation, executed through `action/execute`.
    Act(Action),
    /// A lifecycle call: a method rather than an action.
    Call(Call),
    /// A structured read. The client renders its own view of it.
    Read(Read),
}

/// The two session-lifecycle methods a slash command reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// `session/resume`.
    Resume(SessionLocator),
    /// `session/close` — `/exit`, `/quit`.
    Close,
}

/// What a viewing command reads. Each variant names the method that answers it,
/// so a frontend never has to guess where a view's facts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// `action/list`: the command table itself (`/help`).
    Commands,
    /// `session/read` (`/status`).
    Session,
    /// `session/read`'s context usage (`/context`).
    Context,
    /// `config/read`, whole or by section (`/config`, `/permissions`, `/mcp`).
    Config(Option<ConfigSection>),
    /// `catalog/read` (`/model`, `/provider`, `/skills`, `/images`).
    Catalog(CatalogKind),
    /// `resource/read` (`/tasks`).
    Resource(ResourceKind),
    /// `session/list` (`/resume` with no argument).
    Sessions,
    /// The project's chart and what the crew is doing.
    Team(TeamRead),
    /// One action's argument choices, for a picker (`/theme`, `/think`).
    Options(ActionId),
}

/// The reading half of `/team`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamRead {
    /// The blueprint and who is hired against it.
    Chart,
    /// Who is up, where, and on what.
    Status,
    /// Whether the chart is well formed.
    Validate,
    /// The agreement the crew works under.
    Norms,
    /// What the crew has written down.
    Memory,
}

/// Why a line is not a command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("unknown command: /{0}")]
    Unknown(String),
    /// The name is a command; what followed it is not.
    #[error("usage: /{name} {usage}")]
    Usage {
        name: &'static str,
        usage: &'static str,
    },
}

// ---------------------------------------------------------------------------
// The table's shapes
// ---------------------------------------------------------------------------

/// Where an argument's values come from.
///
/// It stays inside the process: `action/list` publishes the fixed choices, and a
/// client that wants a live list reads the catalog itself. What it buys is that
/// the terminal's argument dropdown is declared beside the command rather than in
/// a second `match` that knew a quarter of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentSource {
    /// Free text: a path, a name, a message.
    Free,
    /// The values listed in the spec.
    Fixed,
    /// `catalog/read` of this kind.
    Catalog(CatalogKind),
    /// `resource/read` of this kind.
    Resource(ResourceKind),
    /// `session/list`.
    Sessions,
    /// The rooms the session has.
    Rooms,
    /// The crew named by the project's chart.
    Crew,
}

/// One argument of one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentSpec {
    pub name: &'static str,
    pub kind: ArgumentKind,
    pub required: bool,
    pub description: &'static str,
    pub source: ArgumentSource,
    pub choices: &'static [&'static str],
}

impl ArgumentSpec {
    const fn free(name: &'static str, required: bool, description: &'static str) -> Self {
        Self {
            name,
            kind: ArgumentKind::String,
            required,
            description,
            source: ArgumentSource::Free,
            choices: &[],
        }
    }

    const fn path(name: &'static str, required: bool, description: &'static str) -> Self {
        Self {
            name,
            kind: ArgumentKind::Path,
            required,
            description,
            source: ArgumentSource::Free,
            choices: &[],
        }
    }

    const fn choice(
        name: &'static str,
        required: bool,
        description: &'static str,
        choices: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            kind: ArgumentKind::Enumeration,
            required,
            description,
            source: ArgumentSource::Fixed,
            choices,
        }
    }

    const fn from(
        name: &'static str,
        required: bool,
        description: &'static str,
        source: ArgumentSource,
    ) -> Self {
        Self {
            name,
            kind: ArgumentKind::Enumeration,
            required,
            description,
            source,
            choices: &[],
        }
    }

    fn published(&self) -> ActionArgument {
        ActionArgument {
            name: self.name.to_string(),
            kind: self.kind,
            required: self.required,
            description: self.description.to_string(),
            choices: self.choices.iter().map(|c| (*c).to_string()).collect(),
        }
    }
}

/// What has to be true before an action can run.
///
/// Availability is state, not taste: `action/list` answers it per page, and
/// `action/execute` refuses on the same rule rather than on a second copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requires {
    /// The console must not be running a turn: the action rewrites what that
    /// turn is reading.
    pub idle_console: bool,
    /// The engine half of the session — a model, a transcript rewrite, or a
    /// network round trip. A core started without one says so rather than
    /// accepting work it cannot do.
    pub engine: bool,
}

impl Requires {
    const NOTHING: Self = Self {
        idle_console: false,
        engine: false,
    };
    const IDLE: Self = Self {
        idle_console: true,
        engine: false,
    };
    const ENGINE: Self = Self {
        idle_console: false,
        engine: true,
    };
    const IDLE_ENGINE: Self = Self {
        idle_console: true,
        engine: true,
    };
}

/// What the core knows when it answers "can this run right now?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Availability {
    pub console_busy: bool,
    pub engine_attached: bool,
}

impl Requires {
    /// `None` when the action is available; otherwise the short English reason.
    pub fn unmet(self, state: Availability) -> Option<&'static str> {
        if self.idle_console && state.console_busy {
            return Some("the console is running a turn");
        }
        if self.engine && !state.engine_attached {
            return Some("this session has no engine attached");
        }
        None
    }
}

/// One action's published metadata. `action/list` reads this; so does the
/// dispatcher, which is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    pub id: &'static str,
    pub family: ActionFamily,
    /// A short English label, for a menu entry.
    pub label: &'static str,
    pub description: &'static str,
    pub arguments: &'static [ArgumentSpec],
    /// The revision a mutation of this action carries, when it needs one.
    pub precondition: Option<RevisionScope>,
    pub requires: Requires,
    /// How a user reaches it from the terminal.
    pub reach: Reach,
}

/// How the CLI reaches an action. The completeness test reads this: an action
/// that claims a command must have one, and one that claims not to must say why
/// it is different.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A command in the table produces it.
    Command,
    /// A name the table cannot hold: a skill is a command called by its own
    /// name, so `/guide` resolves against the session's skills rather than
    /// against a fixed entry.
    Dynamic,
    /// The terminal's gesture is not a typed line — a key on a running row.
    Gesture,
}

impl ActionSpec {
    /// What `action/list` publishes for this action on one page.
    pub fn published(&self, state: Availability) -> ActionInfo {
        let unavailable = self.requires.unmet(state);
        ActionInfo {
            id: ActionId(self.id.to_string()),
            family: self.family,
            label: self.label.to_string(),
            description: self.description.to_string(),
            available: unavailable.is_none(),
            unavailable_reason: unavailable.map(str::to_string),
            arguments: self.arguments.iter().map(ArgumentSpec::published).collect(),
            precondition_scope: self.precondition,
        }
    }
}

/// One command: its name, the names it answers to, and what its line means.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    /// Other spellings that dispatch to the same parser. They were dispatch-only
    /// before, which is how `/quit` came to queue behind a turn while `/exit`
    /// did too and `/help` did not.
    pub aliases: &'static [&'static str],
    /// The argument hint shown after the name.
    pub hint: &'static str,
    pub description: &'static str,
    pub family: ActionFamily,
    /// It runs while a turn is running rather than waiting behind it: a settings
    /// knob applies before the next turn, and a read has nothing to wait for.
    pub instant: bool,
    pub arguments: &'static [ArgumentSpec],
    /// Every action id this command's parser can produce.
    pub produces: &'static [&'static str],
    /// Read the text after the name.
    pub parse: fn(&str) -> Result<Command, ParseError>,
}

impl CommandSpec {
    /// `/name hint`, as `/help` and the dropdown print it.
    pub fn usage(&self) -> String {
        if self.hint.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.hint)
        }
    }
}

// ---------------------------------------------------------------------------
// The action table
// ---------------------------------------------------------------------------

const THINKING_CHOICES: &[&str] = &["off", "low", "medium", "high", "xhigh", "max"];
const THEME_CHOICES: &[&str] = &["auto", "dark", "light"];
const PERMISSION_DECISIONS: &[&str] = &["allow", "deny", "ask"];
const PERMISSION_MODES: &[&str] = &[
    "default",
    "acceptEdits",
    "bypassPermissions",
    "dontAsk",
    "plan",
];

const NO_ARGUMENTS: &[ArgumentSpec] = &[];

/// Every action, once, with what it is called and when it can run.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        id: "session.reset",
        family: ActionFamily::Session,
        label: "New session",
        description: "clear the conversation and start a new session",
        arguments: NO_ARGUMENTS,
        precondition: None,
        requires: Requires::IDLE_ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "session.rename",
        family: ActionFamily::Session,
        label: "Rename session",
        description: "rename the current session",
        arguments: &[ArgumentSpec::free("name", true, "the new name")],
        precondition: Some(RevisionScope::Session),
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "session.gc",
        family: ActionFamily::Session,
        label: "Clean session storage",
        description: "clean expired session data (30-day TTL; latest 100 inactive kept)",
        arguments: NO_ARGUMENTS,
        precondition: None,
        requires: Requires::IDLE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "session.share",
        family: ActionFamily::Session,
        label: "Share session",
        description: "export the session as HTML; --public publishes a link",
        arguments: &[
            ArgumentSpec::choice("public", false, "publish a public link", &["--public"]),
            ArgumentSpec::choice("open", false, "open the result afterwards", &["--open"]),
        ],
        precondition: None,
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "session.cd",
        family: ActionFamily::Session,
        label: "Change directory",
        description: "switch the session working directory",
        arguments: &[ArgumentSpec::path("path", true, "the new directory")],
        precondition: Some(RevisionScope::Session),
        requires: Requires::IDLE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "conversation.compact",
        family: ActionFamily::Conversation,
        label: "Compact context",
        description: "summarise the older half of the context",
        arguments: &[ArgumentSpec::free(
            "instructions",
            false,
            "what the summary should keep",
        )],
        precondition: None,
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "conversation.rewind",
        family: ActionFamily::Conversation,
        label: "Rewind",
        description: "go back to an earlier checkpoint",
        arguments: &[ArgumentSpec::choice(
            "mode",
            false,
            "preview the checkpoint or apply it",
            &["preview", "applied"],
        )],
        precondition: Some(RevisionScope::Conversation),
        requires: Requires::IDLE_ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "model.select",
        family: ActionFamily::Model,
        label: "Select model",
        description: "switch the model this session runs on",
        arguments: &[ArgumentSpec::from(
            "model",
            true,
            "the model id",
            ArgumentSource::Catalog(CatalogKind::Models),
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::IDLE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "provider.select",
        family: ActionFamily::Provider,
        label: "Select provider",
        description: "switch the API provider",
        arguments: &[ArgumentSpec::from(
            "provider",
            true,
            "the provider name",
            ArgumentSource::Catalog(CatalogKind::Providers),
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::IDLE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "provider.login",
        family: ActionFamily::Provider,
        label: "Authenticate provider",
        description: "sign in to a provider",
        arguments: &[ArgumentSpec::from(
            "provider",
            true,
            "the provider name",
            ArgumentSource::Catalog(CatalogKind::Providers),
        )],
        precondition: None,
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "provider.logout",
        family: ActionFamily::Provider,
        label: "Sign out of provider",
        description: "drop a provider's stored credentials",
        arguments: &[ArgumentSpec::from(
            "provider",
            true,
            "the provider name",
            ArgumentSource::Catalog(CatalogKind::Providers),
        )],
        precondition: None,
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "thinking.select",
        family: ActionFamily::Model,
        label: "Set thinking level",
        description: "how much the model may think before answering",
        arguments: &[ArgumentSpec::choice(
            "level",
            true,
            "the thinking level",
            THINKING_CHOICES,
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "permission.mode",
        family: ActionFamily::Permission,
        label: "Set permission mode",
        description: "how much the session asks before acting",
        arguments: &[ArgumentSpec::choice(
            "mode",
            true,
            "the permission mode",
            PERMISSION_MODES,
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "permission.ruleAdd",
        family: ActionFamily::Permission,
        label: "Add permission rule",
        description: "add a permission rule",
        arguments: &[
            ArgumentSpec::choice("decision", true, "allow, deny or ask", PERMISSION_DECISIONS),
            ArgumentSpec::free("rule", true, "the rule, as Tool(content)"),
        ],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "permission.ruleRemove",
        family: ActionFamily::Permission,
        label: "Remove permission rule",
        description: "remove a permission rule",
        arguments: &[
            ArgumentSpec::choice("decision", true, "allow, deny or ask", PERMISSION_DECISIONS),
            ArgumentSpec::free("rule", true, "the rule, as Tool(content)"),
        ],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "mcp.enable",
        family: ActionFamily::Mcp,
        label: "Enable MCP server",
        description: "enable a configured MCP server",
        arguments: &[ArgumentSpec::from(
            "server",
            true,
            "the server name",
            ArgumentSource::Catalog(CatalogKind::McpServers),
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "mcp.disable",
        family: ActionFamily::Mcp,
        label: "Disable MCP server",
        description: "disable a configured MCP server",
        arguments: &[ArgumentSpec::from(
            "server",
            true,
            "the server name",
            ArgumentSource::Catalog(CatalogKind::McpServers),
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "mcp.reconnect",
        family: ActionFamily::Mcp,
        label: "Reconnect MCP",
        description: "reconnect one MCP server, or all of them",
        arguments: &[ArgumentSpec::from(
            "server",
            false,
            "the server name; absent reconnects every one",
            ArgumentSource::Catalog(CatalogKind::McpServers),
        )],
        precondition: None,
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "skill.invoke",
        family: ActionFamily::Skill,
        label: "Run skill",
        // Reached as `/<skill name>`: the table cannot hold names it learns from
        // the project, so the parser resolves them against the session's skills.
        description: "hand a skill to the model",
        arguments: &[
            ArgumentSpec::from(
                "skill",
                true,
                "the skill name",
                ArgumentSource::Catalog(CatalogKind::Skills),
            ),
            ArgumentSpec::free("input", false, "what to pass it"),
        ],
        precondition: None,
        requires: Requires::NOTHING,
        reach: Reach::Dynamic,
    },
    ActionSpec {
        id: "team.start",
        family: ActionFamily::Team,
        label: "Start team",
        description: "bring the project's crew up",
        arguments: &[ArgumentSpec::from(
            "members",
            false,
            "who to start; absent starts the whole chart",
            ArgumentSource::Crew,
        )],
        precondition: Some(RevisionScope::Team),
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "team.assign",
        family: ActionFamily::Team,
        label: "Assign work",
        description: "hand a member a piece of work",
        arguments: &[
            ArgumentSpec::from("member", true, "who to hand it to", ArgumentSource::Crew),
            ArgumentSpec::free("task", true, "what to do"),
        ],
        precondition: None,
        requires: Requires::ENGINE,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "team.stop",
        family: ActionFamily::Team,
        label: "Stop team",
        description: "stand the crew down",
        arguments: &[ArgumentSpec::from(
            "member",
            false,
            "who to stop; absent stops the whole team",
            ArgumentSource::Crew,
        )],
        precondition: Some(RevisionScope::Team),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "team.scaffold",
        family: ActionFamily::Team,
        label: "Create team",
        description: "write a starting chart and its agreement into the project",
        arguments: &[ArgumentSpec::free("name", true, "what to call the team")],
        precondition: Some(RevisionScope::Team),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "team.memoryGc",
        family: ActionFamily::Team,
        label: "Clean team memory",
        description: "drop the crew's notes past their 30-day life",
        arguments: NO_ARGUMENTS,
        precondition: None,
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "room.join",
        family: ActionFamily::Room,
        label: "Join room",
        description: "join a room so you can speak in it",
        arguments: &[ArgumentSpec::from(
            "room",
            true,
            "the room name",
            ArgumentSource::Rooms,
        )],
        precondition: Some(RevisionScope::Rooms),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "room.leave",
        family: ActionFamily::Room,
        label: "Leave room",
        description: "leave a room; it stays readable",
        arguments: &[ArgumentSpec::from(
            "room",
            true,
            "the room name",
            ArgumentSource::Rooms,
        )],
        precondition: Some(RevisionScope::Rooms),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
    ActionSpec {
        id: "command.promote",
        family: ActionFamily::Command,
        label: "Send to background",
        description: "let the foreground command keep running out of the way",
        arguments: &[ArgumentSpec::free("itemId", true, "the command's item")],
        precondition: None,
        requires: Requires::NOTHING,
        // The terminal's gesture for this is a key on a running command's row,
        // not a typed line. A GUI gets it through `action/execute` like any
        // other.
        reach: Reach::Gesture,
    },
    ActionSpec {
        id: "theme.set",
        family: ActionFamily::Settings,
        label: "Set theme",
        description: "switch the theme",
        arguments: &[ArgumentSpec::choice(
            "theme",
            true,
            "the theme",
            THEME_CHOICES,
        )],
        precondition: Some(RevisionScope::Config),
        requires: Requires::NOTHING,
        reach: Reach::Command,
    },
];

/// One action's metadata, by its stable id.
pub fn action_spec(id: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|spec| spec.id == id)
}

/// The metadata for an action value. Dispatch and `action/list` read the same
/// function, so a renamed variant cannot keep an old menu entry alive.
pub fn spec_of(action: &Action) -> &'static ActionSpec {
    let id = action.id();
    action_spec(id.as_str()).unwrap_or_else(|| {
        // The completeness test below makes this unreachable; the panic states
        // the invariant rather than papering over a hole in the table.
        panic!("{id} has no entry in the action table")
    })
}

/// What `action/list` publishes, in table order.
pub fn published(state: Availability) -> Vec<ActionInfo> {
    ACTIONS.iter().map(|spec| spec.published(state)).collect()
}

// ---------------------------------------------------------------------------
// The command table
// ---------------------------------------------------------------------------

/// Every slash command, in the order `/help` prints them.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        aliases: &["?"],
        hint: "",
        description: "show available commands",
        family: ActionFamily::Session,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Commands)),
    },
    CommandSpec {
        name: "clear",
        aliases: &["reset", "new"],
        hint: "",
        description: "clear the conversation and start a new session",
        family: ActionFamily::Session,
        instant: false,
        arguments: NO_ARGUMENTS,
        produces: &["session.reset"],
        parse: |_| Ok(Command::Act(Action::SessionReset)),
    },
    CommandSpec {
        name: "compact",
        aliases: &[],
        hint: "[instructions]",
        description: "compact the context: older messages become a summary",
        family: ActionFamily::Conversation,
        instant: false,
        arguments: &[ArgumentSpec::free(
            "instructions",
            false,
            "what the summary should keep",
        )],
        produces: &["conversation.compact"],
        parse: |rest| {
            Ok(Command::Act(Action::ConversationCompact {
                instructions: optional(rest),
            }))
        },
    },
    CommandSpec {
        name: "rewind",
        aliases: &[],
        hint: "[preview|apply]",
        description: "go back to an earlier checkpoint",
        family: ActionFamily::Conversation,
        instant: false,
        arguments: &[ArgumentSpec::choice(
            "mode",
            false,
            "preview the checkpoint or apply it",
            &["preview", "apply"],
        )],
        produces: &["conversation.rewind"],
        parse: |rest| {
            let mode = match rest.trim() {
                "" | "preview" => RewindMode::Preview,
                "apply" | "applied" => RewindMode::Applied,
                _ => {
                    return Err(ParseError::Usage {
                        name: "rewind",
                        usage: "[preview|apply]",
                    });
                }
            };
            Ok(Command::Act(Action::ConversationRewind {
                target: RewindTarget::Latest,
                mode,
            }))
        },
    },
    CommandSpec {
        name: "model",
        aliases: &[],
        hint: "[name]",
        description: "show/switch the model",
        family: ActionFamily::Model,
        instant: true,
        arguments: &[ArgumentSpec::from(
            "model",
            false,
            "the model id",
            ArgumentSource::Catalog(CatalogKind::Models),
        )],
        produces: &["model.select"],
        parse: |rest| match optional(rest) {
            Some(model) => Ok(Command::Act(Action::ModelSelect { model })),
            None => Ok(Command::Read(Read::Catalog(CatalogKind::Models))),
        },
    },
    CommandSpec {
        name: "cd",
        aliases: &[],
        hint: "<dir>",
        description: "switch the session working directory",
        family: ActionFamily::Session,
        instant: false,
        arguments: &[ArgumentSpec::path("path", true, "the new directory")],
        produces: &["session.cd"],
        parse: |rest| match optional(rest) {
            Some(path) => Ok(Command::Act(Action::SessionChangeDirectory {
                path: PathBuf::from(path),
            })),
            None => Err(ParseError::Usage {
                name: "cd",
                usage: "<dir>",
            }),
        },
    },
    CommandSpec {
        name: "resume",
        aliases: &[],
        hint: "[name or keyword]",
        description: "resume a past session",
        family: ActionFamily::Session,
        instant: false,
        arguments: &[ArgumentSpec::from(
            "session",
            false,
            "the session's name",
            ArgumentSource::Sessions,
        )],
        produces: &[],
        parse: |rest| match optional(rest) {
            Some(stem) => Ok(Command::Call(Call::Resume(SessionLocator::Stem { stem }))),
            None => Ok(Command::Read(Read::Sessions)),
        },
    },
    CommandSpec {
        name: "rename",
        aliases: &[],
        hint: "<name>",
        description: "rename the current session",
        family: ActionFamily::Session,
        instant: false,
        arguments: &[ArgumentSpec::free("name", true, "the new name")],
        produces: &["session.rename"],
        parse: |rest| match optional(rest) {
            Some(name) => Ok(Command::Act(Action::SessionRename { name })),
            None => Err(ParseError::Usage {
                name: "rename",
                usage: "<name>",
            }),
        },
    },
    CommandSpec {
        name: "gc",
        aliases: &[],
        hint: "",
        description: "clean expired session data (30-day TTL; latest 100 inactive sessions kept)",
        family: ActionFamily::Session,
        // It deletes storage the running turn may be writing to, so unlike the
        // other instant commands it waits: it used to skip the queue and then
        // refuse itself mid-turn, which is a queue rule stated twice and
        // obeyed once.
        instant: false,
        arguments: NO_ARGUMENTS,
        produces: &["session.gc"],
        parse: |_| Ok(Command::Act(Action::SessionGarbageCollect)),
    },
    CommandSpec {
        name: "share",
        aliases: &[],
        hint: "[--public] [--open]",
        description: "export HTML; --public publishes a public link",
        family: ActionFamily::Session,
        instant: false,
        arguments: &[
            ArgumentSpec::choice("public", false, "publish a public link", &["--public"]),
            ArgumentSpec::choice("open", false, "open the result afterwards", &["--open"]),
        ],
        produces: &["session.share"],
        parse: |rest| {
            let mut public = false;
            let mut open = false;
            let mut output = None;
            for token in rest.split_whitespace() {
                match token {
                    "--public" => public = true,
                    "--open" => open = true,
                    other if other.starts_with('-') => {
                        return Err(ParseError::Usage {
                            name: "share",
                            usage: "[--public] [--open] [path]",
                        });
                    }
                    path => output = Some(PathBuf::from(path)),
                }
            }
            Ok(Command::Act(Action::SessionShare {
                public,
                open,
                output,
            }))
        },
    },
    CommandSpec {
        name: "context",
        aliases: &[],
        hint: "",
        description: "show context usage",
        family: ActionFamily::Session,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Context)),
    },
    CommandSpec {
        name: "status",
        aliases: &[],
        hint: "",
        description: "show session status (model/permissions/session/context)",
        family: ActionFamily::Session,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Session)),
    },
    CommandSpec {
        name: "config",
        aliases: &[],
        hint: "",
        description: "show the effective config and its sources (layer/env vars/endpoint)",
        family: ActionFamily::Settings,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Config(None))),
    },
    CommandSpec {
        name: "permissions",
        aliases: &[],
        hint: "[allow|deny|ask <rule>] [remove allow|deny|ask <rule>]",
        description: "list, add or remove permission rules",
        family: ActionFamily::Permission,
        instant: true,
        arguments: &[
            ArgumentSpec::choice(
                "decision",
                false,
                "allow, deny, ask, or remove",
                &["allow", "deny", "ask", "remove"],
            ),
            ArgumentSpec::free("rule", false, "the rule, as Tool(content)"),
        ],
        produces: &["permission.ruleAdd", "permission.ruleRemove"],
        parse: parse_permissions,
    },
    CommandSpec {
        name: "mode",
        aliases: &[],
        hint: "[default|acceptEdits|bypassPermissions|dontAsk|plan]",
        description: "how much the session asks before acting",
        family: ActionFamily::Permission,
        instant: true,
        arguments: &[ArgumentSpec::choice(
            "mode",
            false,
            "the permission mode",
            PERMISSION_MODES,
        )],
        produces: &["permission.mode"],
        parse: |rest| match optional(rest) {
            None => Ok(Command::Read(Read::Options(ActionId(
                "permission.mode".to_string(),
            )))),
            Some(name) => match permission_mode(&name) {
                Some(mode) => Ok(Command::Act(Action::PermissionModeSet { mode })),
                None => Err(ParseError::Usage {
                    name: "mode",
                    usage: "[default|acceptEdits|bypassPermissions|dontAsk|plan]",
                }),
            },
        },
    },
    CommandSpec {
        name: "theme",
        aliases: &[],
        hint: "[dark|light|auto]",
        description: "switch the theme",
        family: ActionFamily::Settings,
        instant: true,
        arguments: &[ArgumentSpec::choice(
            "theme",
            false,
            "the theme",
            THEME_CHOICES,
        )],
        produces: &["theme.set"],
        parse: |rest| match optional(rest) {
            None => Ok(Command::Read(Read::Options(ActionId(
                "theme.set".to_string(),
            )))),
            Some(name) => match theme_choice(&name) {
                Some(theme) => Ok(Command::Act(Action::ThemeSet { theme })),
                None => Err(ParseError::Usage {
                    name: "theme",
                    usage: "[dark|light|auto]",
                }),
            },
        },
    },
    CommandSpec {
        name: "images",
        aliases: &[],
        hint: "",
        description: "open an image from this session",
        family: ActionFamily::Session,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Catalog(CatalogKind::Images))),
    },
    CommandSpec {
        name: "mcp",
        aliases: &[],
        hint: "[enable|disable|reconnect] [name]",
        description: "manage MCP servers",
        family: ActionFamily::Mcp,
        instant: true,
        arguments: &[
            ArgumentSpec::choice(
                "operation",
                false,
                "what to do",
                &["enable", "disable", "reconnect"],
            ),
            ArgumentSpec::from(
                "server",
                false,
                "the server name",
                ArgumentSource::Catalog(CatalogKind::McpServers),
            ),
        ],
        produces: &["mcp.enable", "mcp.disable", "mcp.reconnect"],
        parse: parse_mcp,
    },
    CommandSpec {
        name: "provider",
        aliases: &[],
        hint: "[name|login <name>|logout <name>]",
        description: "list/switch API providers, or sign in and out of one",
        family: ActionFamily::Provider,
        instant: true,
        arguments: &[
            ArgumentSpec::from(
                "provider",
                false,
                "the provider name, or login/logout",
                ArgumentSource::Catalog(CatalogKind::Providers),
            ),
            ArgumentSpec::from(
                "target",
                false,
                "which provider to sign in or out of",
                ArgumentSource::Catalog(CatalogKind::Providers),
            ),
        ],
        produces: &["provider.select", "provider.login", "provider.logout"],
        parse: parse_provider,
    },
    CommandSpec {
        name: "think",
        aliases: &[],
        hint: "[off|low|medium|high|xhigh|max]",
        description: "set the thinking level",
        family: ActionFamily::Model,
        instant: true,
        arguments: &[ArgumentSpec::choice(
            "level",
            false,
            "the thinking level",
            THINKING_CHOICES,
        )],
        produces: &["thinking.select"],
        parse: |rest| match optional(rest) {
            None => Ok(Command::Read(Read::Options(ActionId(
                "thinking.select".to_string(),
            )))),
            Some(name) => match thinking_level(&name) {
                Some(level) => Ok(Command::Act(Action::ThinkingSelect { level })),
                None => Err(ParseError::Usage {
                    name: "think",
                    usage: "[off|low|medium|high|xhigh|max]",
                }),
            },
        },
    },
    CommandSpec {
        name: "skills",
        aliases: &[],
        hint: "",
        description: "list available skills",
        family: ActionFamily::Skill,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Catalog(CatalogKind::Skills))),
    },
    CommandSpec {
        name: "tasks",
        aliases: &[],
        hint: "",
        description: "list background tasks",
        family: ActionFamily::Session,
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Read(Read::Resource(ResourceKind::Tasks))),
    },
    CommandSpec {
        name: "team",
        aliases: &[],
        hint: "list|start|status|assign|stop|validate|new|norms|memory",
        description: "manage the project team",
        family: ActionFamily::Team,
        instant: false,
        arguments: &[
            ArgumentSpec::choice(
                "subcommand",
                false,
                "what to do",
                &[
                    "list", "start", "status", "assign", "stop", "validate", "new", "norms",
                    "memory",
                ],
            ),
            ArgumentSpec::free("rest", false, "who, and what to say to them"),
        ],
        produces: &[
            "team.start",
            "team.assign",
            "team.stop",
            "team.scaffold",
            "team.memoryGc",
        ],
        parse: parse_team,
    },
    CommandSpec {
        name: "join",
        aliases: &[],
        hint: "#room",
        description: "join a room so you can speak in it",
        family: ActionFamily::Room,
        instant: false,
        arguments: &[ArgumentSpec::from(
            "room",
            true,
            "the room name",
            ArgumentSource::Rooms,
        )],
        produces: &["room.join"],
        parse: |rest| match room_name(rest) {
            Some(room) => Ok(Command::Act(Action::RoomJoin { room })),
            None => Err(ParseError::Usage {
                name: "join",
                usage: "#room",
            }),
        },
    },
    CommandSpec {
        name: "leave",
        aliases: &[],
        hint: "#room",
        description: "leave a room (it stays readable)",
        family: ActionFamily::Room,
        instant: false,
        arguments: &[ArgumentSpec::from(
            "room",
            true,
            "the room name",
            ArgumentSource::Rooms,
        )],
        produces: &["room.leave"],
        parse: |rest| match room_name(rest) {
            Some(room) => Ok(Command::Act(Action::RoomLeave { room })),
            None => Err(ParseError::Usage {
                name: "leave",
                usage: "#room",
            }),
        },
    },
    CommandSpec {
        name: "exit",
        aliases: &["quit"],
        hint: "",
        description: "exit the session",
        family: ActionFamily::Session,
        // Leaving does not wait behind a turn. It used to, because `exit` was
        // never added to the instant list.
        instant: true,
        arguments: NO_ARGUMENTS,
        produces: &[],
        parse: |_| Ok(Command::Call(Call::Close)),
    },
];

/// One command by the name or alias it was typed as.
pub fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name))
}

/// Whether a command line runs while a turn is running rather than waiting
/// behind it.
///
/// The rule is the line's meaning, not its name. A read has nothing to wait for;
/// a mutation waits unless applying it before the next turn is the whole point
/// of it. Two bugs fall out of stating it once: `/?` used to queue while `/help`
/// did not, because the instant list held names and the dispatcher held aliases;
/// and `/gc` used to skip the queue and then refuse itself mid-turn, which is
/// one rule written twice and obeyed once.
pub fn is_instant(line: &str) -> bool {
    match parse(line) {
        Ok(Command::Read(_)) => true,
        // An unreadable line is answered, not queued: the error belongs to the
        // moment it was typed.
        Err(_) => true,
        Ok(_) => command_name(line)
            .is_some_and(|name| command_spec(name).is_some_and(|spec| spec.instant)),
    }
}

/// The command word of a line, before any argument.
fn command_name(line: &str) -> Option<&str> {
    let line = line.trim();
    let name = match line.split_once(char::is_whitespace) {
        Some((name, _)) => name,
        None => line,
    };
    (!name.is_empty()).then_some(name)
}

/// Read one command line — everything after the leading slash, which the
/// composer has already stripped.
pub fn parse(line: &str) -> Result<Command, ParseError> {
    parse_in(line, &[])
}

/// Read one command line against the skills this session has.
///
/// A skill is a command called by its own name (`/guide`), so the table cannot
/// hold it: the names come from the project and the user's config. Resolving
/// them here rather than in a frontend is what makes `/guide` mean the same
/// thing in every client — and what turns the terminal's old "unknown command"
/// fall-through into a typed [`Action::SkillInvoke`].
pub fn parse_in(line: &str, skills: &[String]) -> Result<Command, ParseError> {
    let line = line.trim();
    let (name, rest) = match line.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (line, ""),
    };
    if let Some(spec) = command_spec(name) {
        return (spec.parse)(rest);
    }
    if skills.iter().any(|skill| skill == name) {
        return Ok(Command::Act(Action::SkillInvoke {
            skill: name.to_string(),
            input: optional(rest),
        }));
    }
    Err(ParseError::Unknown(name.to_string()))
}

// ---------------------------------------------------------------------------
// The parsers the table points at
// ---------------------------------------------------------------------------

fn optional(rest: &str) -> Option<String> {
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Read back through the wire spelling, so the argument a user types and the
/// value a client sends are the same word by construction.
fn thinking_level(name: &str) -> Option<ThinkingLevel> {
    ThinkingLevel::ALL
        .into_iter()
        .find(|level| level.as_str() == name)
}

fn permission_mode(name: &str) -> Option<PermissionMode> {
    PermissionMode::ALL
        .into_iter()
        .find(|mode| mode.as_str() == name)
}

fn theme_choice(name: &str) -> Option<ThemeChoice> {
    ThemeChoice::ALL
        .into_iter()
        .find(|theme| theme.as_str() == name)
}

fn rule_decision(name: &str) -> Option<PermissionRuleDecision> {
    Some(match name {
        "allow" => PermissionRuleDecision::Allow,
        "deny" => PermissionRuleDecision::Deny,
        "ask" => PermissionRuleDecision::Ask,
        _ => return None,
    })
}

/// `#build` and `build` are the same room. The sigil is how a room is written,
/// not part of its name.
fn room_name(rest: &str) -> Option<String> {
    let name = rest.trim().trim_start_matches('#').trim();
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_string())
}

const PERMISSIONS_USAGE: &str = "[allow|deny|ask <rule>] · remove <allow|deny|ask> <rule>";

fn parse_permissions(rest: &str) -> Result<Command, ParseError> {
    let Some((head, tail)) = split_word(rest) else {
        return Ok(Command::Read(Read::Config(Some(
            ConfigSection::Permissions,
        ))));
    };
    let usage = ParseError::Usage {
        name: "permissions",
        usage: PERMISSIONS_USAGE,
    };
    if head == "remove" {
        let (kind, rule) = split_word(tail).ok_or_else(|| usage.clone())?;
        let decision = rule_decision(kind).ok_or_else(|| usage.clone())?;
        let rule = optional(rule).ok_or(usage)?;
        return Ok(Command::Act(Action::PermissionRuleRemove {
            decision,
            rule,
        }));
    }
    let decision = rule_decision(head).ok_or_else(|| usage.clone())?;
    let rule = optional(tail).ok_or(usage)?;
    Ok(Command::Act(Action::PermissionRuleAdd { decision, rule }))
}

const MCP_USAGE: &str = "[enable|disable|reconnect] [name]";

fn parse_mcp(rest: &str) -> Result<Command, ParseError> {
    let Some((head, tail)) = split_word(rest) else {
        return Ok(Command::Read(Read::Config(Some(ConfigSection::Mcp))));
    };
    let usage = ParseError::Usage {
        name: "mcp",
        usage: MCP_USAGE,
    };
    match head {
        "reconnect" => Ok(Command::Act(Action::McpReconnect {
            server: optional(tail),
        })),
        "enable" => Ok(Command::Act(Action::McpEnable {
            server: optional(tail).ok_or(usage)?,
        })),
        "disable" => Ok(Command::Act(Action::McpDisable {
            server: optional(tail).ok_or(usage)?,
        })),
        _ => Err(usage),
    }
}

const PROVIDER_USAGE: &str = "[name] · login <name> · logout <name>";

fn parse_provider(rest: &str) -> Result<Command, ParseError> {
    let Some((head, tail)) = split_word(rest) else {
        return Ok(Command::Read(Read::Catalog(CatalogKind::Providers)));
    };
    let usage = ParseError::Usage {
        name: "provider",
        usage: PROVIDER_USAGE,
    };
    match head {
        // The flags the device/manual flows take are the login handler's; what
        // the registry decides is which provider is being signed in to.
        "login" => Ok(Command::Act(Action::ProviderLogin {
            provider: first_word(tail).ok_or(usage)?,
        })),
        "logout" => Ok(Command::Act(Action::ProviderLogout {
            provider: first_word(tail).ok_or(usage)?,
        })),
        name => Ok(Command::Act(Action::ProviderSelect {
            provider: name.to_string(),
        })),
    }
}

const TEAM_USAGE: &str =
    "list|start|status|assign <member> <task>|stop|validate|new <name>|norms|memory [list|gc]";

fn parse_team(rest: &str) -> Result<Command, ParseError> {
    let Some((head, tail)) = split_word(rest) else {
        return Ok(Command::Read(Read::Team(TeamRead::Chart)));
    };
    let usage = ParseError::Usage {
        name: "team",
        usage: TEAM_USAGE,
    };
    match head {
        "list" | "ls" => Ok(Command::Read(Read::Team(TeamRead::Chart))),
        "status" | "st" => Ok(Command::Read(Read::Team(TeamRead::Status))),
        "validate" | "check" => Ok(Command::Read(Read::Team(TeamRead::Validate))),
        "norms" => Ok(Command::Read(Read::Team(TeamRead::Norms))),
        "start" | "up" => Ok(Command::Act(Action::TeamStart {
            members: members(tail),
        })),
        "stop" | "down" => Ok(Command::Act(Action::TeamStop {
            member: first_word(tail),
        })),
        "assign" | "say" => {
            let (member, task) = split_word(tail).ok_or_else(|| usage.clone())?;
            Ok(Command::Act(Action::TeamAssign {
                member: member.to_string(),
                task: optional(task).ok_or(usage)?,
            }))
        }
        "new" => Ok(Command::Act(Action::TeamScaffold {
            name: optional(tail).ok_or(usage)?,
        })),
        "memory" => match optional(tail).as_deref() {
            None | Some("list") | Some("ls") => Ok(Command::Read(Read::Team(TeamRead::Memory))),
            Some("gc") => Ok(Command::Act(Action::TeamMemoryGarbageCollect)),
            Some(_) => Err(usage),
        },
        _ => Err(usage),
    }
}

/// The first word of `rest`, and everything after it.
fn split_word(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    Some(match rest.split_once(char::is_whitespace) {
        Some((head, tail)) => (head, tail.trim()),
        None => (rest, ""),
    })
}

fn first_word(rest: &str) -> Option<String> {
    split_word(rest).map(|(head, _)| head.to_string())
}

fn members(rest: &str) -> Option<Vec<String>> {
    let names: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
    (!names.is_empty()).then_some(names)
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

/// `/help`, straight off the table. The sub-command line that used to be
/// hand-appended here is gone: `/provider login` is in the table's own hint now.
pub fn help_lines() -> Vec<String> {
    let mut lines = vec!["available commands:".to_string()];
    let width = COMMANDS
        .iter()
        .map(|spec| spec.usage().chars().count())
        .max()
        .unwrap_or(0);
    for spec in COMMANDS {
        let usage = spec.usage();
        lines.push(format!("  {usage:<width$} — {}", spec.description));
    }
    lines.push("keys: press ? on an empty input for the full table".to_string());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::command::tests::every_action;

    /// Metadata and dispatch are the same table, and the compiler cannot say so
    /// on its own: the action union is a Rust enum and the specs are data. This
    /// is what says so.
    #[test]
    fn every_action_has_exactly_one_spec() {
        for action in every_action() {
            let id = action.id();
            let spec = action_spec(id.as_str())
                .unwrap_or_else(|| panic!("{id} is an action with no entry in the table"));
            assert_eq!(
                spec.family,
                action.family(),
                "{id} disagrees with itself about which family it belongs to"
            );
        }
        assert_eq!(
            ACTIONS.len(),
            every_action().len(),
            "the table has an entry for an action that no longer exists"
        );
        let mut ids: Vec<&str> = ACTIONS.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len(), "two specs share an id");
    }

    /// Every action a client can be told about is one a user can reach, and
    /// every action a command claims to produce exists.
    #[test]
    fn the_command_table_reaches_every_action_that_has_a_line() {
        let produced: std::collections::BTreeSet<&str> = COMMANDS
            .iter()
            .flat_map(|spec| spec.produces.iter().copied())
            .collect();
        for id in &produced {
            assert!(
                action_spec(id).is_some(),
                "/{id} is produced by a command and defined nowhere"
            );
        }
        for spec in ACTIONS {
            assert_eq!(
                spec.reach == Reach::Command,
                produced.contains(spec.id),
                "{} claims {:?} and the command table disagrees",
                spec.id,
                spec.reach
            );
        }
        let exceptions: Vec<(&str, Reach)> = ACTIONS
            .iter()
            .filter(|spec| spec.reach != Reach::Command)
            .map(|spec| (spec.id, spec.reach))
            .collect();
        assert_eq!(
            exceptions,
            vec![
                ("skill.invoke", Reach::Dynamic),
                ("command.promote", Reach::Gesture),
            ],
            "an action with no entry in the table needs a reason, not a default"
        );
    }

    /// A skill is a command whose name the table cannot know, and the core is
    /// what resolves it: the terminal's "unknown command" fall-through used to
    /// be the only place `/guide` meant anything.
    #[test]
    fn a_skill_is_a_command_called_by_its_own_name() {
        let skills = vec!["guide".to_string()];
        assert_eq!(
            parse_in("guide the lexer", &skills),
            Ok(Command::Act(Action::SkillInvoke {
                skill: "guide".to_string(),
                input: Some("the lexer".to_string()),
            }))
        );
        assert_eq!(
            parse_in("guide", &skills),
            Ok(Command::Act(Action::SkillInvoke {
                skill: "guide".to_string(),
                input: None,
            }))
        );
        assert_eq!(
            parse_in("guide", &[]),
            Err(ParseError::Unknown("guide".to_string())),
            "a skill this session does not have is not a command"
        );
        assert_eq!(
            parse_in("help", &["help".to_string()]),
            Ok(Command::Read(Read::Commands)),
            "the table wins: a skill cannot shadow a command"
        );
    }

    /// Names and aliases are one namespace. Two commands answering to the same
    /// word is how a dispatch arm and a table entry come apart.
    #[test]
    fn no_two_commands_answer_to_the_same_word() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in COMMANDS {
            for word in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
                assert!(seen.insert(word), "two commands answer to /{word}");
                assert!(
                    word.chars().all(|c| c.is_ascii_alphanumeric() || c == '?'),
                    "/{word} is not a typeable command name"
                );
            }
        }
    }

    /// An alias is the same command, which includes waiting — or not waiting —
    /// behind a running turn. `/?` used to queue while `/help` did not.
    #[test]
    fn an_alias_is_the_same_command_as_its_name() {
        for spec in COMMANDS {
            for alias in spec.aliases {
                assert_eq!(
                    is_instant(alias),
                    is_instant(spec.name),
                    "/{alias} and /{} disagree about the queue",
                    spec.name
                );
                assert_eq!(
                    command_spec(alias).map(|found| found.name),
                    Some(spec.name),
                    "/{alias} resolves somewhere else"
                );
            }
        }
    }

    /// Every command parses its own empty line into something, and every
    /// command that takes a required argument says so rather than inventing one.
    #[test]
    fn every_command_reads_its_own_line() {
        for spec in COMMANDS {
            let bare = parse(spec.name);
            let required = spec.arguments.iter().any(|argument| argument.required);
            match (&bare, required) {
                (Ok(_), false) | (Err(ParseError::Usage { .. }), true) => {}
                other => panic!("/{} with no argument: {other:?}", spec.name),
            }
            assert!(
                !spec.description.is_empty(),
                "/{} needs a short English description",
                spec.name
            );
        }
    }

    #[test]
    fn a_command_line_becomes_the_action_it_names() {
        assert_eq!(
            parse("theme dark"),
            Ok(Command::Act(Action::ThemeSet {
                theme: ThemeChoice::Dark
            }))
        );
        assert_eq!(
            parse("think max"),
            Ok(Command::Act(Action::ThinkingSelect {
                level: ThinkingLevel::Max
            }))
        );
        assert_eq!(
            parse("join #build"),
            Ok(Command::Act(Action::RoomJoin {
                room: "build".to_string()
            })),
            "the sigil is how a room is written, not part of its name"
        );
        assert_eq!(
            parse("provider login codex"),
            Ok(Command::Act(Action::ProviderLogin {
                provider: "codex".to_string()
            })),
            "a sub-command the old table never listed"
        );
        assert_eq!(
            parse("permissions remove allow Bash(cargo test:*)"),
            Ok(Command::Act(Action::PermissionRuleRemove {
                decision: PermissionRuleDecision::Allow,
                rule: "Bash(cargo test:*)".to_string(),
            })),
            "the wire had this action and the CLI had no way to say it"
        );
        assert_eq!(
            parse("team assign scout survey the crate"),
            Ok(Command::Act(Action::TeamAssign {
                member: "scout".to_string(),
                task: "survey the crate".to_string(),
            }))
        );
        assert_eq!(
            parse("share --public --open"),
            Ok(Command::Act(Action::SessionShare {
                public: true,
                open: true,
                output: None,
            }))
        );
    }

    #[test]
    fn a_bare_command_is_a_read_rather_than_a_guess() {
        assert_eq!(parse("help"), Ok(Command::Read(Read::Commands)));
        assert_eq!(parse("?"), Ok(Command::Read(Read::Commands)));
        assert_eq!(parse("status"), Ok(Command::Read(Read::Session)));
        assert_eq!(parse("config"), Ok(Command::Read(Read::Config(None))));
        assert_eq!(
            parse("mcp"),
            Ok(Command::Read(Read::Config(Some(ConfigSection::Mcp))))
        );
        assert_eq!(
            parse("permissions"),
            Ok(Command::Read(Read::Config(Some(
                ConfigSection::Permissions
            ))))
        );
        assert_eq!(
            parse("model"),
            Ok(Command::Read(Read::Catalog(CatalogKind::Models)))
        );
        assert_eq!(parse("resume"), Ok(Command::Read(Read::Sessions)));
        assert_eq!(
            parse("team"),
            Ok(Command::Read(Read::Team(TeamRead::Chart)))
        );
        assert_eq!(
            parse("theme"),
            Ok(Command::Read(Read::Options(ActionId(
                "theme.set".to_string()
            )))),
            "a picker is that action's own argument choices"
        );
    }

    #[test]
    fn exit_and_resume_are_lifecycle_calls_rather_than_actions() {
        assert_eq!(parse("exit"), Ok(Command::Call(Call::Close)));
        assert_eq!(parse("quit"), Ok(Command::Call(Call::Close)));
        assert_eq!(
            parse("resume notes-1"),
            Ok(Command::Call(Call::Resume(SessionLocator::Stem {
                stem: "notes-1".to_string()
            })))
        );
    }

    #[test]
    fn an_unknown_command_is_named_rather_than_guessed_at() {
        assert_eq!(
            parse("nonesuch"),
            Err(ParseError::Unknown("nonesuch".to_string()))
        );
        assert_eq!(
            parse("theme chartreuse"),
            Err(ParseError::Usage {
                name: "theme",
                usage: "[dark|light|auto]"
            })
        );
    }

    /// Which lines skip a running turn is one rule now, and it reads the line's
    /// meaning rather than its first word.
    #[test]
    fn a_read_never_waits_behind_a_turn() {
        for spec in COMMANDS {
            if matches!((spec.parse)(""), Ok(Command::Read(_))) {
                assert!(
                    is_instant(spec.name),
                    "/{} shows something and has nothing to wait for",
                    spec.name
                );
            }
        }
        assert!(is_instant("resume"), "listing the sessions is a read");
        assert!(
            !is_instant("resume notes-1"),
            "switching session is not, and the name is the same"
        );
        assert!(
            !is_instant("gc"),
            "it deletes storage the running turn may be writing to"
        );
        assert!(is_instant("?"), "an alias is the same command");
        assert!(is_instant("exit"), "leaving does not wait for a turn");
    }

    #[test]
    fn availability_is_answered_from_state_rather_than_from_taste() {
        let idle = Availability {
            console_busy: false,
            engine_attached: true,
        };
        let busy = Availability {
            console_busy: true,
            engine_attached: true,
        };
        let bare = Availability {
            console_busy: false,
            engine_attached: false,
        };
        let cd = action_spec("session.cd").unwrap_or_else(|| panic!("session.cd exists"));
        assert!(cd.published(idle).available);
        let published = cd.published(busy);
        assert!(!published.available);
        assert_eq!(
            published.unavailable_reason.as_deref(),
            Some("the console is running a turn")
        );
        let compact =
            action_spec("conversation.compact").unwrap_or_else(|| panic!("compact exists"));
        assert!(!compact.published(bare).available);
        let theme = action_spec("theme.set").unwrap_or_else(|| panic!("theme.set exists"));
        assert!(
            theme.published(bare).available,
            "a preference needs nothing"
        );
    }

    #[test]
    fn help_is_the_table_and_nothing_else() {
        let lines = help_lines();
        assert_eq!(lines.len(), COMMANDS.len() + 2, "title + commands + keys");
        for (spec, line) in COMMANDS.iter().zip(&lines[1..]) {
            assert!(line.contains(&spec.usage()), "{line}");
            assert!(line.ends_with(spec.description), "{line}");
        }
    }
}
