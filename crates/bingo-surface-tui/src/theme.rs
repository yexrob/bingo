//! The glyphs and styles the whole surface draws with, in one place, so a
//! change of look is a change of one file rather than a hunt through the views.

use ratatui::style::{Color, Modifier, Style};

/// Prefix of a user's own line in the transcript.
pub const USER: &str = "❯ ";
/// Prefix of a tool row.
pub const TOOL: &str = "● ";
/// Reasoning, collapsed to one line, and the attention marker in the title.
pub const THINKING: &str = "✻ ";
/// Terminal bell, written out of band between frames.
pub const BELL: &[u8] = b"\x07";

pub const DONE: &str = "✓";
pub const FAILED: &str = "✗";
pub const STOPPED: &str = "⊘";

/// Braille spinner, one frame every [`SPINNER_MS`].
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPINNER_MS: u128 = 80;

/// The spinner frame for an elapsed duration, so the animation is a pure
/// function of the clock rather than a counter someone has to advance.
pub fn spinner(elapsed: std::time::Duration) -> &'static str {
    let index = (elapsed.as_millis() / SPINNER_MS) as usize % SPINNER.len();
    SPINNER[index]
}

pub fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn italic() -> Style {
    Style::default().add_modifier(Modifier::ITALIC)
}

pub fn plain() -> Style {
    Style::default()
}

pub fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn danger() -> Style {
    Style::default().fg(Color::Red)
}

pub fn caution() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn good() -> Style {
    Style::default().fg(Color::Green)
}

/// The row a list or dropdown has focus on.
pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub fn level(level: bingo_sdk::Level) -> Style {
    match level {
        bingo_sdk::Level::Info => dim(),
        bingo_sdk::Level::Warn => caution(),
        bingo_sdk::Level::Error => danger(),
    }
}
