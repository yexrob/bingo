//! The attention channel: bell, desktop notification, terminal title.
//!
//! A user who switches away from the terminal has no way of learning that the
//! turn finished or that a permission prompt is waiting. This module builds the
//! bytes that tell them; it never writes them. Emission belongs to
//! [`crate::tui::term`], the single owner of escape-sequence writes, so the
//! payload reaches the terminal between frames instead of racing the viewport
//! diff.
//!
//! Three things are carried:
//!
//! - a **notification**, addressed to the desktop (`OSC 9` iTerm2, `OSC 99`
//!   kitty, `OSC 777` Ghostty) or, when nothing better is known, the terminal
//!   **bell**;
//! - a **terminal title** (`OSC 2`) tracking busy / waiting / idle, so the tab
//!   answers "is it my turn yet?" without switching to it;
//! - nothing at all, when the channel is [`NotifyChannel::Disabled`].
//!
//! **tmux.** A notification OSC is wrapped in tmux's passthrough envelope
//! (shared with the image transport, [`crate::tui::gfx`]) because tmux does not
//! know those sequences and swallows them. The bell is left bare — tmux has its
//! own bell action (`monitor-bell`), which only fires on a bell it can see. The
//! title is left bare too, and for the same kind of reason: OSC 2 is a sequence
//! tmux *does* understand, sets as the pane title and propagates to the window
//! title under `set-titles on`. Sending it through passthrough would set the
//! outer terminal's title behind tmux's back, and tmux would overwrite it on its
//! next redraw.

use crate::tui::gfx::{Transport, tmux_passthrough};

/// A turn shorter than this finished while the user was still watching; a
/// notification for it would be noise.
pub const LONG_TURN: std::time::Duration = std::time::Duration::from_secs(10);

/// BEL — the notification OSC terminator, and the whole payload of
/// [`NotifyChannel::Bell`].
const BEL: u8 = 0x07;

/// The notification title every channel that has one carries.
const APP: &str = "bingo";

/// Attention channel (settings key `notifications`).
///
/// [`NotifyChannel::Auto`] is a request to decide from the terminal, not a
/// channel: [`NotifyChannel::resolve`] turns it into one of the others before a
/// [`Notifier`] ever stores it. A stray `Auto` reaching the byte builders
/// behaves as `Bell`, which is what auto-detection falls back to anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyChannel {
    /// Decide from the terminal (the settings default).
    Auto,
    /// The terminal bell.
    Bell,
    /// iTerm2's `OSC 9` message.
    Iterm2,
    /// kitty's three-part `OSC 99` desktop notification.
    Kitty,
    /// Ghostty's `OSC 777 ; notify` desktop notification.
    Ghostty,
    /// Nothing is emitted — no notification, no title.
    Disabled,
}

impl NotifyChannel {
    /// Parse the settings string; a missing or unrecognized value is `auto`
    /// (the same leniency `theme` and `motion` grant).
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or("") {
            "bell" => Self::Bell,
            "iterm2" => Self::Iterm2,
            "kitty" => Self::Kitty,
            "ghostty" => Self::Ghostty,
            "off" | "disabled" | "none" => Self::Disabled,
            _ => Self::Auto,
        }
    }

    /// Resolve `Auto` against the terminal; every other value is the user's
    /// explicit choice and passes through untouched.
    ///
    /// The matrix is deliberately narrow: only terminals that identify
    /// themselves get their own protocol, and everything else gets the bell,
    /// which every terminal has. There is no probe — a notification protocol has
    /// no query, so a wrong guess would be silence.
    pub fn resolve(self, env: &TerminalEnv) -> Self {
        if self != Self::Auto {
            return self;
        }
        match env.term_program.as_deref() {
            Some("iTerm.app") => Self::Iterm2,
            Some("ghostty") => Self::Ghostty,
            Some("kitty") => Self::Kitty,
            _ if env.term.as_deref() == Some("xterm-kitty") => Self::Kitty,
            _ => Self::Bell,
        }
    }
}

/// The terminal facts auto-detection needs, read once at startup.
///
/// Library code does not read the environment (it would make every test depend
/// on the shell that launched it), so the TUI entry point resolves these and
/// hands them down — the same sealing `Session::user_config_dir` applies to the
/// config directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalEnv {
    pub term_program: Option<String>,
    pub term: Option<String>,
    pub in_tmux: bool,
}

impl TerminalEnv {
    /// Read the environment. Called once, from the TUI entry point.
    pub fn from_env() -> Self {
        Self {
            term_program: std::env::var("TERM_PROGRAM").ok(),
            term: std::env::var("TERM").ok(),
            in_tmux: std::env::var_os("TMUX").is_some(),
        }
    }

    /// How a notification OSC reaches the terminal that shows it.
    pub fn transport(&self) -> Transport {
        if self.in_tmux {
            Transport::Tmux
        } else {
            Transport::Bare
        }
    }
}

/// What the user is being told. The bodies are one line each, in English, and
/// say only what is certain at the moment they fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// A permission prompt is on screen and the turn is blocked on it.
    WaitingPermission,
    /// A turn that ran long enough for the user to have walked away is done.
    TurnComplete,
    /// A turn ended in a flow-level failure.
    TurnFailed,
}

impl Attention {
    pub fn body(self) -> &'static str {
        match self {
            Self::WaitingPermission => "Waiting for permission",
            Self::TurnComplete => "Turn complete",
            Self::TurnFailed => "Turn failed",
        }
    }
}

/// What the terminal title says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Title<'a> {
    /// A turn is running. The glyph is the `title_glyph` motion token (D87):
    /// `✳` and the braille half-cells alternating about once a second, or a
    /// static `✳` when motion is off. The title is the one surface a user sees
    /// while looking at another window, so it moves slowly and deliberately.
    Busy(char),
    /// A permission prompt is waiting for an answer.
    WaitingPermission,
    /// Nothing is running; the title names where the session is.
    Idle(&'a str),
}

impl Title<'_> {
    fn text(self) -> String {
        match self {
            Self::Busy(glyph) => format!("{glyph} bingo — working…"),
            Self::WaitingPermission => "✳ bingo — waiting for permission".to_string(),
            Self::Idle(cwd) => format!("bingo — {cwd}"),
        }
    }
}

/// The directory name a title shows: the last component of `cwd`, which is what
/// a tab has room for and what the user calls the project. A path with no last
/// component (a filesystem root) shows whole.
pub fn cwd_short(cwd: &str) -> &str {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(cwd)
}

/// Builds attention bytes and holds them until the term layer collects them.
///
/// Nothing here writes: [`Notifier::take`] hands the accumulated payload to the
/// host, which emits it through the term layer between frames.
#[derive(Debug)]
pub struct Notifier {
    /// Resolved channel — never [`NotifyChannel::Auto`].
    channel: NotifyChannel,
    transport: Transport,
    /// kitty addresses a notification by id; a fresh one per notification keeps
    /// them from replacing each other.
    next_id: u32,
    out: Vec<u8>,
    /// The title last emitted, so an unchanged state costs no bytes (a turn end
    /// and an idle redraw would otherwise repaint the same title).
    title: Option<String>,
}

impl Default for Notifier {
    /// Silent. A `Chat` built without a host (every test, the entity modal)
    /// emits nothing; the TUI entry point installs the configured channel.
    fn default() -> Self {
        Self::new(NotifyChannel::Disabled, &TerminalEnv::default())
    }
}

impl Notifier {
    /// Resolve `channel` against `env` and build a notifier for it.
    pub fn new(channel: NotifyChannel, env: &TerminalEnv) -> Self {
        Self {
            channel: channel.resolve(env),
            transport: env.transport(),
            next_id: 1,
            out: Vec::new(),
            title: None,
        }
    }

    /// Whether this notifier emits anything at all.
    pub fn enabled(&self) -> bool {
        self.channel != NotifyChannel::Disabled
    }

    /// Queue a desktop notification (or a bell).
    pub fn attention(&mut self, what: Attention) {
        if self.channel == NotifyChannel::Disabled {
            return;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let bytes = notification_bytes(self.channel, id, APP, what.body());
        // The bell is not an OSC: tmux forwards it and acts on it, and a
        // passthrough envelope would hide it from `monitor-bell`.
        if self.channel == NotifyChannel::Bell {
            self.out.extend_from_slice(&bytes);
        } else {
            self.out.extend_from_slice(&wrap(self.transport, &bytes));
        }
    }

    /// Queue a terminal title, unless it already says this.
    pub fn set_title(&mut self, title: Title<'_>) {
        if self.channel == NotifyChannel::Disabled {
            return;
        }
        let text = title.text();
        if self.title.as_deref() == Some(text.as_str()) {
            return;
        }
        self.out.extend_from_slice(&title_bytes(&text));
        self.title = Some(text);
    }

    /// Take the pending bytes; the host hands them to the term layer.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

/// The bytes that hand the title back on the way out: an empty `OSC 2`, which
/// terminals treat as "no title of mine" and shells overwrite at the next
/// prompt. Bare, like every other title (see the module docs on tmux).
pub const RESET_TITLE: &[u8] = b"\x1b]2;\x07";

/// `OSC 2 ; {text} BEL`.
fn title_bytes(text: &str) -> Vec<u8> {
    let mut out = b"\x1b]2;".to_vec();
    out.extend_from_slice(sanitize(text).as_bytes());
    out.push(BEL);
    out
}

/// One notification, unwrapped. Split out from [`Notifier`] so the goldens can
/// be asserted without a notifier's state.
fn notification_bytes(channel: NotifyChannel, id: u32, title: &str, body: &str) -> Vec<u8> {
    let title = sanitize(title);
    let body = sanitize(body);
    match channel {
        NotifyChannel::Disabled => Vec::new(),
        // Auto is resolved before it gets here; bell is what it resolves to
        // when the terminal says nothing about itself.
        NotifyChannel::Auto | NotifyChannel::Bell => vec![BEL],
        // iTerm2's OSC 9 carries a single line and no title.
        NotifyChannel::Iterm2 => format!("\x1b]9;{body}\x07").into_bytes(),
        // kitty splits a notification into parts sharing an id: the title, the
        // body, and the `d=1` part that says it is complete.
        NotifyChannel::Kitty => format!(
            "\x1b]99;i={id}:d=0:p=title;{title}\x07\
             \x1b]99;i={id}:d=0:p=body;{body}\x07\
             \x1b]99;i={id}:d=1;\x07"
        )
        .into_bytes(),
        NotifyChannel::Ghostty => format!("\x1b]777;notify;{title};{body}\x07").into_bytes(),
    }
}

fn wrap(transport: Transport, bytes: &[u8]) -> Vec<u8> {
    match transport {
        Transport::Bare => bytes.to_vec(),
        Transport::Tmux => tmux_passthrough(bytes),
    }
}

/// Drop control characters. Everything embedded here is either a constant or a
/// path component, and a stray ESC or BEL in a path would end the sequence
/// early and spill the rest onto the screen as text.
fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(program: Option<&str>, term: Option<&str>, in_tmux: bool) -> TerminalEnv {
        TerminalEnv {
            term_program: program.map(str::to_string),
            term: term.map(str::to_string),
            in_tmux,
        }
    }

    fn notify(channel: NotifyChannel, env: &TerminalEnv, what: Attention) -> Vec<u8> {
        let mut n = Notifier::new(channel, env);
        n.attention(what);
        n.take()
    }

    /// Golden bytes per channel. These are a contract with four terminals that
    /// cannot be asked whether they understood: a wrong byte is silence, not an
    /// error, so the sequences are pinned here rather than eyeballed.
    #[test]
    fn every_channel_has_its_own_sequence() {
        let bare = env(None, None, false);
        assert_eq!(
            notify(NotifyChannel::Bell, &bare, Attention::TurnComplete),
            b"\x07".to_vec(),
            "the bell is one byte and nothing else"
        );
        assert_eq!(
            notify(NotifyChannel::Iterm2, &bare, Attention::TurnComplete),
            b"\x1b]9;Turn complete\x07".to_vec()
        );
        assert_eq!(
            notify(NotifyChannel::Kitty, &bare, Attention::WaitingPermission),
            b"\x1b]99;i=1:d=0:p=title;bingo\x07\
              \x1b]99;i=1:d=0:p=body;Waiting for permission\x07\
              \x1b]99;i=1:d=1;\x07"
                .to_vec(),
            "kitty takes three parts sharing one id"
        );
        assert_eq!(
            notify(NotifyChannel::Ghostty, &bare, Attention::TurnFailed),
            b"\x1b]777;notify;bingo;Turn failed\x07".to_vec()
        );
    }

    /// Every kitty notification gets an id of its own, otherwise the second one
    /// would replace the first instead of arriving beside it.
    #[test]
    fn kitty_notifications_do_not_share_an_id() {
        let mut n = Notifier::new(NotifyChannel::Kitty, &env(None, None, false));
        n.attention(Attention::WaitingPermission);
        n.attention(Attention::TurnComplete);
        let out = String::from_utf8_lossy(&n.take()).to_string();
        assert!(out.contains("i=1:d=1;"), "first notification: {out:?}");
        assert!(out.contains("i=2:d=1;"), "second notification: {out:?}");
    }

    /// tmux swallows the notification OSCs (it has never heard of 9/99/777), so
    /// they travel in the passthrough envelope. The bell does not: tmux acts on
    /// a bell it can see, and passthrough would hide it from `monitor-bell`.
    #[test]
    fn tmux_wraps_the_osc_and_leaves_the_bell_alone() {
        let tmux = env(None, None, true);
        assert_eq!(
            notify(NotifyChannel::Bell, &tmux, Attention::TurnComplete),
            b"\x07".to_vec(),
            "a wrapped bell would never reach the outer terminal's bell action"
        );
        assert_eq!(
            notify(NotifyChannel::Iterm2, &tmux, Attention::TurnComplete),
            b"\x1bPtmux;\x1b\x1b]9;Turn complete\x07\x1b\\".to_vec(),
            "the envelope doubles the payload's ESC"
        );

        // The title is a sequence tmux understands: it becomes the pane title
        // and tmux propagates it. Wrapped, it would set the outer terminal's
        // title behind tmux's back and be overwritten on the next redraw.
        let mut n = Notifier::new(NotifyChannel::Iterm2, &tmux);
        n.set_title(Title::Busy('✳'));
        assert_eq!(
            n.take(),
            "\x1b]2;✳ bingo — working…\x07".as_bytes().to_vec()
        );
    }

    /// Auto is the only value that reads the terminal; an explicit choice is
    /// never second-guessed.
    #[test]
    fn auto_detection_matrix() {
        let cases: [(Option<&str>, Option<&str>, NotifyChannel); 7] = [
            (Some("iTerm.app"), None, NotifyChannel::Iterm2),
            (Some("ghostty"), None, NotifyChannel::Ghostty),
            (Some("kitty"), None, NotifyChannel::Kitty),
            (None, Some("xterm-kitty"), NotifyChannel::Kitty),
            (
                Some("Apple_Terminal"),
                Some("xterm-256color"),
                NotifyChannel::Bell,
            ),
            (Some("vscode"), None, NotifyChannel::Bell),
            (None, None, NotifyChannel::Bell),
        ];
        for (program, term, expect) in cases {
            assert_eq!(
                NotifyChannel::Auto.resolve(&env(program, term, false)),
                expect,
                "TERM_PROGRAM={program:?} TERM={term:?}"
            );
        }
        // tmux overwrites TERM_PROGRAM in its panes; the bell is the honest
        // answer there and it is what the fallback gives.
        assert_eq!(
            NotifyChannel::Auto.resolve(&env(Some("tmux"), Some("screen-256color"), true)),
            NotifyChannel::Bell
        );
        // An explicit channel survives a terminal that says otherwise.
        for explicit in [
            NotifyChannel::Bell,
            NotifyChannel::Iterm2,
            NotifyChannel::Kitty,
            NotifyChannel::Ghostty,
            NotifyChannel::Disabled,
        ] {
            assert_eq!(
                explicit.resolve(&env(Some("iTerm.app"), None, false)),
                explicit
            );
        }
    }

    #[test]
    fn settings_strings_map_onto_channels() {
        assert_eq!(NotifyChannel::parse(None), NotifyChannel::Auto);
        assert_eq!(NotifyChannel::parse(Some("auto")), NotifyChannel::Auto);
        assert_eq!(NotifyChannel::parse(Some(" bell ")), NotifyChannel::Bell);
        assert_eq!(NotifyChannel::parse(Some("iterm2")), NotifyChannel::Iterm2);
        assert_eq!(NotifyChannel::parse(Some("kitty")), NotifyChannel::Kitty);
        assert_eq!(
            NotifyChannel::parse(Some("ghostty")),
            NotifyChannel::Ghostty
        );
        assert_eq!(NotifyChannel::parse(Some("off")), NotifyChannel::Disabled);
        assert_eq!(
            NotifyChannel::parse(Some("disabled")),
            NotifyChannel::Disabled
        );
        assert_eq!(
            NotifyChannel::parse(Some("nonsense")),
            NotifyChannel::Auto,
            "an unrecognized value falls back rather than turning the channel off"
        );
    }

    /// Disabled is silent on every surface, including the title — a user who
    /// turned notifications off did not ask for their tab to be renamed either.
    #[test]
    fn disabled_emits_nothing() {
        let mut n = Notifier::new(NotifyChannel::Disabled, &env(Some("iTerm.app"), None, true));
        assert!(!n.enabled());
        n.attention(Attention::WaitingPermission);
        n.attention(Attention::TurnComplete);
        n.set_title(Title::Busy('✳'));
        n.set_title(Title::Idle("bingo"));
        assert!(n.take().is_empty(), "disabled writes no bytes at all");
        // The default notifier is the disabled one: a Chat with no host attached
        // must not emit either.
        let mut d = Notifier::default();
        d.attention(Attention::TurnFailed);
        d.set_title(Title::Busy('✳'));
        assert!(d.take().is_empty());
    }

    #[test]
    fn the_title_tracks_the_three_states_and_repeats_nothing() {
        let mut n = Notifier::new(NotifyChannel::Bell, &env(None, None, false));
        n.set_title(Title::Busy('✳'));
        n.set_title(Title::WaitingPermission);
        n.set_title(Title::Idle("bingo"));
        assert_eq!(
            String::from_utf8_lossy(&n.take()),
            "\x1b]2;✳ bingo — working…\x07\
             \x1b]2;✳ bingo — waiting for permission\x07\
             \x1b]2;bingo — bingo\x07"
        );
        n.set_title(Title::Idle("bingo"));
        assert!(
            n.take().is_empty(),
            "the same title twice is one title's worth of bytes"
        );
    }

    /// A path is not a constant: an ESC in a directory name would close the
    /// sequence and print the rest of it on screen.
    #[test]
    fn control_characters_never_reach_the_terminal() {
        let mut n = Notifier::new(NotifyChannel::Bell, &env(None, None, false));
        n.set_title(Title::Idle("we\x1b]0;pwnedird"));
        assert_eq!(
            String::from_utf8_lossy(&n.take()),
            "\x1b]2;bingo — we]0;pwnedird\x07"
        );
    }

    #[test]
    fn the_title_names_the_directory_not_the_path() {
        assert_eq!(cwd_short("/Users/x/Projects/bingo"), "bingo");
        assert_eq!(cwd_short("/Users/x/Projects/bingo/"), "bingo");
        assert_eq!(cwd_short("bingo"), "bingo");
        assert_eq!(cwd_short("/"), "/", "a root has no last component");
        assert_eq!(cwd_short(""), "");
    }
}
