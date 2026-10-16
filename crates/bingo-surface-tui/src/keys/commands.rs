//! Key spelling and the bindings projected from catalogues and settings.
use crate::ui::Ui;
use bingo_sdk::{SessionState, keys::Bindings};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const STATIC: &[&str] = &[
    "esc",
    "ctrl+c",
    "ctrl+d",
    "enter",
    "shift+enter",
    "alt+enter",
    "ctrl+j",
    "tab",
    "shift+tab",
    "backspace",
    "delete",
    "left",
    "right",
    "up",
    "down",
    "home",
    "end",
    "ctrl+a",
    "ctrl+e",
    "ctrl+w",
    "ctrl+u",
    "ctrl+k",
    "alt+b",
    "alt+f",
    "pgup",
    "pgdn",
    "ctrl+v",
    "ctrl+t",
    "ctrl+o",
    "ctrl+b",
    "alt+p",
    "?",
];

pub fn chord(key: KeyEvent) -> Option<String> {
    if !(key.modifiers & !(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT))
        .is_empty()
    {
        return None;
    }
    let name = match key.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab | KeyCode::BackTab => "tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdn".into(),
        KeyCode::F(n) => format!("f{n}"),
        _ => return None,
    };
    let mut parts = Vec::new();
    for (modifier, name) in [
        (KeyModifiers::CONTROL, "ctrl"),
        (KeyModifiers::ALT, "alt"),
        (KeyModifiers::SHIFT, "shift"),
    ] {
        if key.modifiers.contains(modifier)
            || (modifier == KeyModifiers::SHIFT && key.code == KeyCode::BackTab)
        {
            parts.push(name);
        }
    }
    parts.push(&name);
    Some(parts.join("+"))
}

pub fn bindings(ui: &Ui, state: &SessionState) -> Bindings {
    let settings: std::collections::BTreeMap<String, String> =
        serde_json::from_value(state.config.kernel["keys"].clone()).unwrap_or_default();
    let commands = ui.catalogs.specs();
    let static_keys: Vec<_> = STATIC
        .iter()
        .copied()
        .chain(
            settings
                .keys()
                .map(String::as_str)
                .chain(
                    commands
                        .iter()
                        .flat_map(|command| &command.keys)
                        .map(bingo_sdk::KeyBinding::chord),
                )
                .filter(|key| editing(key)),
        )
        .collect();
    bingo_sdk::keys::bindings(&commands, &settings, &static_keys)
}

fn editing(key: &str) -> bool {
    let parts: Vec<_> = key.split('+').collect();
    let Some(last) = parts.last() else {
        return false;
    };
    if parts.contains(&"ctrl") {
        return STATIC.contains(&format!("ctrl+{last}").as_str());
    }
    if parts.contains(&"alt") {
        return STATIC.contains(&format!("alt+{last}").as_str());
    }
    last.chars().count() == 1 || STATIC.contains(last)
}

pub fn line(ui: &Ui, state: &SessionState, key: KeyEvent) -> Option<String> {
    bindings(ui, state).commands.get(&chord(key)?).cloned()
}

/// The single chords the palette can replay after releasing focus.
pub fn event(key: &str) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::empty();
    let mut parts = key.split('+').collect::<Vec<_>>();
    let name = parts.pop()?;
    for part in parts {
        modifiers |= match part {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return None,
        };
    }
    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        "tab" => KeyCode::Tab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        _ => {
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    Some(KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editing_and_modifier_variants_cannot_be_rebound() {
        let (ui, _) = crate::test_support::scene();
        let mut state = crate::test_support::state();
        for key in [
            "x",
            "shift+X",
            "ctrl+v",
            "ctrl+alt+c",
            "shift+left",
            "alt+shift+b",
        ] {
            state.config.kernel["keys"] = serde_json::json!({key:"/hello"});
            let result = bindings(&ui, &state);
            assert!(result.commands.is_empty(), "{key}");
            assert_eq!(result.notices.len(), 1, "{key}");
        }
    }
}
