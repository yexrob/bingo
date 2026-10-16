//! The active descendants of the session on screen, derived in tree order.
use crate::{
    clock::Now,
    face, seats, theme,
    tree::{self, Scope, Tree},
};
use bingo_collab::lifecycle::Lifecycle;
use bingo_sdk::SessionState;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

pub fn active(tree: &Tree) -> Vec<&SessionState> {
    let under = tree::covered(tree, Scope::Under(tree.view()));
    tree.sessions()
        .filter(|state| under.contains(&&state.summary.id))
        .filter(|state| {
            matches!(
                Lifecycle::of_state(state),
                Some(Lifecycle::Working | Lifecycle::Asking)
            )
        })
        .collect()
}

pub fn spans(tree: &Tree, width: usize, room: usize, now: Now) -> Vec<Span<'static>> {
    let agents = active(tree);
    let mut out = Vec::new();
    let mut used = 0;
    for (at, state) in agents.iter().enumerate() {
        let entry = entry(tree, state, width < 100, now);
        let cells: usize = entry.iter().map(|s| s.content.width()).sum();
        let remaining = agents.len() - at - 1;
        let reserve = if remaining > 0 {
            format!("  +{remaining}").width()
        } else {
            0
        };
        let gap = if out.is_empty() { 0 } else { 2 };
        if used + gap + cells + reserve > room {
            let overflow = format!("+{}", agents.len() - at);
            if used + gap + overflow.width() <= room {
                if gap > 0 {
                    out.push(Span::raw("  "));
                }
                out.push(Span::styled(overflow, theme::dim()));
            }
            break;
        }
        if gap > 0 {
            out.push(Span::raw("  "));
        }
        used += gap + cells;
        out.extend(entry);
    }
    out
}

fn entry(tree: &Tree, state: &SessionState, narrow: bool, now: Now) -> Vec<Span<'static>> {
    let name = tree::name(state);
    let mut out = vec![Span::styled(
        face::face(&name).sigil(),
        face::pulse(&name, true, now),
    )];
    if narrow {
        return out;
    }
    out.push(Span::styled(format!(" {name}"), face::style(&name)));
    if Lifecycle::of_state(state) == Some(Lifecycle::Asking) {
        out.push(Span::styled(" asking", theme::dim()));
    }
    if let Some(seat) = seats::seat(tree, state).filter(|seat| seat.owes.is_some()) {
        out.push(Span::styled(format!(" owes {}", seat.room), theme::dim()));
    }
    out
}

/// The strip uses the installed command; disabling the mod restores the palette.
pub fn open(ui: &mut crate::ui::Ui, tree: &Tree, now: Now) -> Vec<crate::effect::Effect> {
    if ui.catalogs.commands.iter().any(|entry| entry.id == "crew") {
        crate::input::command_line(ui, tree, now, "/crew".into())
    } else {
        crate::palette::open(ui, bingo_sdk::component::PaletteOpen::Sessions, now)
    }
}

#[cfg(test)]
mod tests;
