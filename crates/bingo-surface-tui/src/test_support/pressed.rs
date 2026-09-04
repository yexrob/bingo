//! The keys and mouse events a test presses: one builder per gesture, so a
//! test reads as what a person did rather than as a struct literal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};

/// A synthetic mouse event at a cell of the screen.
pub fn mouse(kind: crossterm::event::MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub fn click(column: u16, row: u16) -> MouseEvent {
    mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
    )
}

pub fn dragged(column: u16, row: u16) -> MouseEvent {
    mouse(
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column,
        row,
    )
}

pub fn wheel(up: bool, column: u16, row: u16) -> MouseEvent {
    let kind = match up {
        true => crossterm::event::MouseEventKind::ScrollUp,
        false => crossterm::event::MouseEventKind::ScrollDown,
    };
    mouse(kind, column, row)
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn typed(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

pub fn alt(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::ALT)
}

/// What a terminal sends for shift+tab: `BackTab`, with the modifier set.
pub fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}
