//! Client placement and the actions minted for the open palette.
use crate::{
    clock::Now,
    effect::Effect,
    tree::Tree,
    ui::{Open, Ui},
};
use bingo_sdk::{
    component::{PaletteOpen, PaletteProps, Props},
    *,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Line;
use serde_json::json;

#[derive(Clone)]
pub struct Current(pub std::sync::Arc<dyn command::CommandCatalog>);
impl std::fmt::Debug for Current {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandCatalog")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    pub first: PaletteOpen,
    pub id: String,
    pub draft: String,
    pub sessions: Vec<SessionSummary>,
    pub children: Vec<SessionSummary>,
    pub fallback_at: Option<usize>,
}
pub fn open(ui: &mut Ui, first: PaletteOpen, now: Now) -> Vec<Effect> {
    let draft = ui.composer.take();
    ui.composer.set(if first == PaletteOpen::Commands {
        "/"
    } else {
        ""
    });
    ui.focus.show(
        Open::Palette(Opened {
            first,
            id: IntentId::mint().to_string(),
            draft,
            sessions: Vec::new(),
            children: Vec::new(),
            fallback_at: None,
        }),
        now.instant,
    );
    ui.edited();
    vec![Effect::ListStored]
}
pub fn close(ui: &mut Ui, now: Now) {
    if let Open::Palette(open) = ui.focus.open() {
        let draft = open.draft.clone();
        ui.composer.set(&draft);
    }
    ui.focus.close(now.instant);
    ui.dialog.aside = false;
    ui.edited();
}
pub fn active(ui: &Ui) -> bool {
    ui.focus.showing() && matches!(ui.focus.open(), Open::Palette(_))
}
pub fn request(ui: &Ui) -> Option<&str> {
    if let Open::Palette(open) = ui.focus.open() {
        Some(&open.id)
    } else {
        None
    }
}
fn props(ui: &Ui, tree: &Tree, open: &Opened, now: Now) -> PaletteProps {
    let state = tree.viewed();
    let mut catalogs = ui.catalogs.values();
    let commands = command_entries(ui, state, &catalogs);
    let read = sources(ui, ui.composer.text());
    catalogs.retain(|kind, _| read.contains(kind));
    PaletteProps {
        key_hints: crate::keys::BINDINGS
            .iter()
            .map(|binding| (binding.keys.into(), binding.description.into()))
            .collect(),
        query: ui.composer.text().into(),
        open: open.first,
        commands,
        bindings: crate::keys::commands::bindings(ui, state).commands,
        sessions: open
            .sessions
            .iter()
            .filter(|s| !tree.is_root(&s.id))
            .map(|summary| sessions::stored(summary, now.wall))
            .collect(),
        conversation: sessions::conversation(tree, &open.children, now),
        session: tree.view().clone(),
        action: Action {
            name: "surface:palette".into(),
            args: json!({"handler":open.id}),
        },
        catalogs,
    }
}
/// The catalogues the query's command completes from: none before the query
/// reaches its arguments, since every press carries them.
fn sources(ui: &Ui, query: &str) -> Vec<String> {
    let Some((name, _)) = query
        .strip_prefix('/')
        .and_then(|rest| rest.split_once(' '))
    else {
        return Vec::new();
    };
    ui.catalogs
        .specs()
        .into_iter()
        .chain(crate::commands::local_specs())
        .filter(|spec| spec.name == name || spec.aliases.iter().any(|alias| alias == name))
        .flat_map(|spec| {
            spec.args
                .sources()
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect()
}
fn command_entries(
    ui: &Ui,
    state: &SessionState,
    catalogs: &std::collections::BTreeMap<String, Vec<CatalogEntry>>,
) -> Vec<CatalogEntry> {
    let mut commands = ui.catalogs.commands.clone();
    commands.extend(
        crate::commands::local_specs()
            .into_iter()
            .map(crate::commands::entry),
    );
    if let Some(reader) = &ui.catalogs.current {
        let providers = catalogs
            .get("providers")
            .into_iter()
            .flatten()
            .filter_map(|entry| serde_json::from_value(entry.meta["auth"].clone()).ok())
            .collect::<Vec<_>>();
        commands = reader
            .0
            .catalog(
                Catalog {
                    kind: CatalogKind::Commands,
                    entries: commands,
                },
                &command::SessionView {
                    state,
                    providers: &providers,
                    models: catalogs.get("models").map_or(&[], Vec::as_slice),
                },
            )
            .entries;
    }
    commands
}
pub fn draw(
    ui: &Ui,
    tree: &Tree,
    frame: &mut crate::layout::Layout,
    width: u16,
    rows: u16,
    now: Now,
) -> Vec<Line<'static>> {
    let Open::Palette(open) = ui.focus.open() else {
        return Vec::new();
    };
    let props = Props::Palette(Box::new(props(ui, tree, open, now)));
    let key = crate::component::key(tree.viewed(), &open.id, &props, width, rows);
    let answer = key.as_ref().and_then(|key| ui.placed.pulled.display(key));
    if let Some(key) = key {
        frame.pulls.push(key);
    }
    let mut lines = crate::component::draw(&props, answer, usize::from(width), |props, width| {
        fallback::draw(props, open.fallback_at, width, rows)
    });
    if answer.is_some() {
        let previous = frame
            .palette_height
            .as_ref()
            .filter(|(id, _)| id == &open.id)
            .map_or(0, |(_, rows)| *rows);
        let height = previous.max(lines.len()).min(usize::from(rows));
        lines.resize(height, Line::default());
        frame.palette_height = Some((open.id.clone(), height));
    }
    lines
}
fn handler(ui: &Ui) -> Option<Action> {
    let id = request(ui)?;
    let key = ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.request_id == id)?;
    if key.request.props["query"].as_str() != Some(ui.composer.text()) {
        return None;
    }
    let View::Box { layout, .. } = ui.placed.pulled.get(key)? else {
        return None;
    };
    layout.on_key.clone()
}
pub fn waiting(ui: &Ui, tree: &Tree, now: Now) -> bool {
    if !active(ui) {
        return false;
    }
    let Open::Palette(open) = ui.focus.open() else {
        return false;
    };
    if let Some(key) = ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.request_id == open.id)
    {
        return key.request.props["query"].as_str() != Some(ui.composer.text())
            || ui.placed.pulled.get(key).is_none()
            || ui
                .placed
                .pulled
                .current(render::Component::Palette, &open.id, tree.viewed())
                .is_none_or(|(_, waiting)| waiting);
    }
    let (width, rows) = room(ui.shown.canvas.area());
    let props = Props::Palette(Box::new(props(ui, tree, open, now)));
    crate::component::key(tree.viewed(), &open.id, &props, width, rows).is_some()
}

/// The room the palette is drawn in under the box: the frame's width and
/// half its height. Drawing and waiting read it here, so they ask for the
/// same drawing.
pub fn room(area: ratatui::layout::Rect) -> (u16, u16) {
    (area.width, area.height / 2)
}

/// Query editing belongs to the composer; list actions belong to the mod.
pub fn edits_query(ui: &Ui, key: KeyEvent) -> bool {
    use crossterm::event::KeyModifiers as M;
    if !active(ui) {
        return false;
    }
    match key.modifiers - M::SHIFT {
        M::NONE => match key.code {
            KeyCode::Char(_)
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Delete => true,
            KeyCode::Backspace => !matches!(ui.composer.text(), "" | "/"),
            _ => false,
        },
        M::CONTROL => matches!(key.code, KeyCode::Char('a' | 'e' | 'w' | 'u' | 'k')),
        M::ALT => matches!(key.code, KeyCode::Char('b' | 'f')),
        _ => false,
    }
}

pub fn key(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if edits_query(ui, key) {
        return crate::input::palette_edit(ui, tree, key, now);
    }
    if let Some(action) = handler(ui) {
        if let Some(chord) = crate::keys::commands::chord(key)
            && let Some(action) = action.with_value(json!({"key":chord}))
        {
            return vec![Effect::SiteKey { action, key }];
        }
        return Vec::new();
    }
    fallback::key(ui, tree, key, now)
}

/// The palette is where the person composes a command, so the line it runs
/// is theirs and the history keeps it, as it keeps one sent from the box.
fn run(ui: &mut Ui, tree: &Tree, line: &str, vouched: bool, now: Now) -> Vec<Effect> {
    close(ui, now);
    if !vouched && crate::commands::privileged(&ui.catalogs.specs(), line) {
        return refuse(ui, line.into(), now);
    }
    crate::input::typed_line(ui, tree, now, line.into())
}

/// `vouched`: the kernel says the palette's own built-in drew the keys and
/// gave this answer, so a line that grants trust may run from it.
pub fn result(ui: &mut Ui, tree: &Tree, action: &Action, vouched: bool, now: Now) -> Vec<Effect> {
    if action.name != "surface:palette" || action.args["handler"].as_str() != request(ui) {
        return Vec::new();
    }
    let value = &action.args["value"];
    if let Some(text) = value["edit"].as_str() {
        ui.composer.set(text);
        ui.edited();
    }
    if let Some(message) = value["message"].as_str() {
        ui.notify(Level::Info, message, now.instant);
    }
    if let Some(key) = value["key"].as_str().and_then(crate::keys::commands::event) {
        close(ui, now);
        if !vouched && crate::commands::privileged(&ui.catalogs.specs(), ui.composer.text()) {
            return refuse(ui, ui.composer.text().to_string(), now);
        }
        return crate::input::on_key(ui, tree, key, now);
    }
    if value["close"] == true {
        close(ui, now);
    }
    if let Some(line) = value["run"].as_str() {
        return run(ui, tree, line, vouched, now);
    }
    if let Some(id) = value["view"].as_str() {
        close(ui, now);
        return vec![Effect::View(SessionId::from_raw(id))];
    }
    if let Some(id) = value["resume"].as_str() {
        close(ui, now);
        return vec![Effect::Open(SessionSelector::ById {
            id: SessionId::from_raw(id),
        })];
    }
    Vec::new()
}

/// A line only the person may write stays in the composer for them to send.
fn refuse(ui: &mut Ui, line: String, now: Now) -> Vec<Effect> {
    ui.notify(Level::Warn, crate::commands::unvouched(&line), now.instant);
    Vec::new()
}

mod fallback;
pub(crate) mod sessions;
#[cfg(test)]
pub(crate) mod tests;

/// The client keeps its native doors when the built-in command is absent.
pub fn command(ui: &mut Ui, line: &str, now: Now) -> Option<Vec<Effect>> {
    let (name, args) = crate::commands::split(line)?;
    if ui.catalogs.commands.iter().any(|entry| entry.id == name) {
        return None;
    }
    match name {
        "sessions" => {
            ui.notify(
                Level::Info,
                "Sessions is off · enable bingo.mod.sessions to open the directory",
                now.instant,
            );
            Some(vec![])
        }
        "help" => Some(open(ui, PaletteOpen::Keys, now)),
        "resume" if args.is_empty() => Some(open(ui, PaletteOpen::Sessions, now)),
        "resume" => Some(vec![Effect::Open(SessionSelector::ById {
            id: SessionId::from_raw(args),
        })]),
        _ => None,
    }
}
