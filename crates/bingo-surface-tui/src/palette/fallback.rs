//! A usable client when no mod owns this site: names and native navigation.
use super::*;
use serde_json::Value;

fn rows(props: &PaletteProps) -> Vec<(String, Value)> {
    let query = props.query.trim_start_matches('/').to_lowercase();
    match props.open {
        PaletteOpen::Sessions => props
            .conversation
            .iter()
            .map(|s| (s.label.clone(), json!({"view":s.id})))
            .chain(
                props
                    .sessions
                    .iter()
                    .map(|s| (s.label.clone(), json!({"resume":s.id}))),
            )
            .filter(|(name, _)| name.to_lowercase().contains(&query))
            .collect(),
        PaletteOpen::Keys => props
            .key_hints
            .keys()
            .filter(|name| name.contains(&query))
            .map(|key| (key.clone(), Value::Null))
            .collect(),
        PaletteOpen::Commands => props
            .commands
            .iter()
            .filter(|entry| {
                entry.meta["hidden"] != true
                    && entry.meta["foldedInto"].as_str().is_none()
                    && entry.id.starts_with(&query)
            })
            .map(|entry| {
                (
                    format!("/{}", entry.id),
                    json!({"run":format!("/{}", entry.id)}),
                )
            })
            .collect(),
    }
}
fn selected(props: &PaletteProps, rows: &[(String, Value)], at: Option<usize>) -> usize {
    at.unwrap_or_else(|| {
        rows.iter()
            .position(|(_, op)| op["view"].as_str() == Some(props.session.as_str()))
            .unwrap_or(0)
    })
    .min(rows.len().saturating_sub(1))
}
pub fn draw(props: &Props, at: Option<usize>, width: usize, height: u16) -> Vec<Line<'static>> {
    let Props::Palette(props) = props else {
        return vec![];
    };
    let rows = rows(props);
    let at = selected(props, &rows, at);
    let room = usize::from(height).max(1);
    let start = at
        .saturating_sub(room / 2)
        .min(rows.len().saturating_sub(room));
    rows.into_iter()
        .enumerate()
        .skip(start)
        .take(room)
        .map(|(i, (label, _))| {
            Line::from(crate::status::clip(
                vec![ratatui::text::Span::raw(format!(
                    " {} {label}",
                    if i == at { "❯" } else { " " }
                ))],
                width,
            ))
        })
        .collect()
}
pub fn key(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let Open::Palette(open) = ui.focus.open() else {
        return vec![];
    };
    let props = super::props(ui, tree, open, now);
    let rows = rows(&props);
    let at = selected(&props, &rows, open.fallback_at);
    match key.code {
        KeyCode::Esc => close(ui, now),
        KeyCode::Char('c')
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL) =>
        {
            close(ui, now)
        }
        KeyCode::Up | KeyCode::Down => {
            if let Some(Open::Palette(open)) = ui.focus.open_mut() {
                open.fallback_at = Some(if key.code == KeyCode::Up {
                    at.saturating_sub(1)
                } else {
                    (at + 1).min(rows.len().saturating_sub(1))
                });
            }
        }
        KeyCode::Enter => {
            if props.open == PaletteOpen::Commands {
                ui.focus.close(now.instant);
                ui.dialog.aside = false;
                return crate::input::palette_submit(ui, tree, now);
            }
            if let Some((_, value)) = rows.get(at)
                && let Some(action) = props.action.with_value(value.clone())
            {
                // The client drew these rows itself; no mod answered.
                return result(ui, tree, &action, true, now);
            }
        }
        _ => {
            if let Some(Open::Palette(open)) = ui.focus.open_mut() {
                open.fallback_at = None;
            }
            return crate::input::palette_edit(ui, tree, key, now);
        }
    }
    vec![]
}
