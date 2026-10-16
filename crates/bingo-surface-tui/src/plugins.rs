//! The Plugins site's placement and client-minted command-line action.
use crate::{
    clock::Now,
    effect::Effect,
    tree::Tree,
    ui::{Open, Ui},
};
use bingo_sdk::{
    component::{PluginsOpen, PluginsProps, Props},
    *,
};
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;
#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    pub id: String,
    pub first: PluginsOpen,
    pub draft: String,
}
pub fn active(ui: &Ui) -> bool {
    ui.focus.showing() && matches!(ui.focus.open(), Open::Plugins(_))
}
pub fn request(ui: &Ui) -> Option<&str> {
    match ui.focus.open() {
        Open::Plugins(open) => Some(&open.id),
        _ => None,
    }
}
pub fn open(ui: &mut Ui, first: PluginsOpen, now: Now) {
    let draft = ui.composer.take();
    ui.focus.show(
        Open::Plugins(Opened {
            id: IntentId::mint().to_string(),
            first,
            draft,
        }),
        now.instant,
    );
    ui.edited();
}
pub(crate) fn close(ui: &mut Ui, now: Now) {
    if let Open::Plugins(open) = ui.focus.open() {
        let draft = open.draft.clone();
        ui.composer.set(&draft);
    }
    ui.focus.close(now.instant);
    ui.edited();
}
fn props(ui: &Ui, tree: &Tree, open: &Opened) -> Props {
    Props::Plugins(PluginsProps {
        open: open.first,
        session: tree.view().clone(),
        plugins: ui.catalogs.values().remove("plugins").unwrap_or_default(),
        action: Action {
            name: "surface:plugins".into(),
            args: json!({"handler":open.id}),
        },
    })
}
pub fn draw(
    ui: &Ui,
    tree: &Tree,
    frame: &mut crate::layout::Layout,
    width: u16,
    rows: u16,
) -> Vec<ratatui::text::Line<'static>> {
    let Open::Plugins(open) = ui.focus.open() else {
        return vec![];
    };
    let props = props(ui, tree, open);
    let key = crate::component::key(tree.viewed(), &open.id, &props, width, rows);
    let answer = key.as_ref().and_then(|key| ui.placed.pulled.get(key));
    if let Some(key) = key {
        frame.pulls.push(key);
    }
    crate::component::draw(&props, answer, usize::from(width), |props, width| {
        let Props::Plugins(props) = props else {
            return vec![];
        };
        let view = View::Table {
            headers: vec!["plugin".into(), "state".into(), "from".into()],
            rows: props
                .plugins
                .iter()
                .map(|entry| {
                    vec![
                        entry.id.clone(),
                        entry.meta["state"]["kind"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                        entry.meta["from"].as_str().unwrap_or_default().into(),
                    ]
                })
                .collect(),
        };
        crate::views::render(&view, width)
    })
}
fn handler(ui: &Ui) -> Option<Action> {
    let id = request(ui)?;
    let key = ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.request_id == id)?;
    let View::Box { layout, .. } = ui.placed.pulled.get(key)? else {
        return None;
    };
    layout.on_key.clone()
}
pub fn waiting(ui: &Ui) -> bool {
    active(ui)
        && ui.shown.pulls.iter().any(|key| {
            Some(key.request.request_id.as_str()) == request(ui)
                && ui.placed.pulled.get(key).is_none()
        })
}
pub fn key(ui: &mut Ui, key: KeyEvent, now: Now) -> Vec<Effect> {
    if let Some(action) = handler(ui) {
        if let Some(chord) = crate::keys::commands::chord(key)
            && let Some(action) = action.with_value(json!({"key":chord}))
        {
            return vec![Effect::SiteKey { action, key }];
        }
    } else if key.code == KeyCode::Esc {
        close(ui, now);
    }
    vec![]
}
/// `vouched`: the kernel says the panel's own built-in drew the keys and
/// gave this answer, so a line that grants trust may run from it.
pub fn result(ui: &mut Ui, tree: &Tree, action: &Action, vouched: bool, now: Now) -> Vec<Effect> {
    if action.name != "surface:plugins" || action.args["handler"].as_str() != request(ui) {
        return vec![];
    }
    let value = &action.args["value"];
    if value["close"] == true {
        close(ui, now);
    }
    if let Some(message) = value["message"].as_str() {
        ui.notify(Level::Info, message, now.instant);
    }
    if let Some(line) = value["run"].as_str() {
        if !vouched && crate::commands::privileged(&ui.catalogs.specs(), line) {
            ui.notify(Level::Warn, crate::commands::unvouched(line), now.instant);
            return vec![];
        }
        if line.starts_with("/mcp login ") || line.starts_with("/mcp tools ") {
            close(ui, now);
        }
        return crate::input::command_line(ui, tree, now, line.into());
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::render::{Component, RenderRequest, SurfaceName, Viewport};
    use crossterm::event::KeyModifiers;

    fn panel(args: serde_json::Value) -> (Ui, Now) {
        let now = Now::real();
        let mut ui = Ui::new(vec![], now.instant);
        open(&mut ui, PluginsOpen::Plugins, now);
        let request = RenderRequest {
            component: Component::Plugins,
            request_id: request(&ui).unwrap().into(),
            surface: SurfaceName::Terminal,
            props: json!({}),
            viewport: Viewport {
                place: None,
                columns: 80,
                rows: 24,
            },
        };
        let key = crate::pulled::Key::new(&crate::test_support::state(), &request, 80, 24);
        ui.shown.pulls.push(key.clone());
        ui.placed.pulled.request(&key);
        ui.placed.pulled.answer(
            key,
            serde_json::from_value(json!({
                "kind":"Box","onKey":{"name":"probe:ui","args":args},"children":[]
            }))
            .unwrap(),
        );
        (ui, now)
    }
    #[test]
    fn malformed_key_arguments_are_refused() {
        for args in [json!(false), json!(42), json!("text"), json!([])] {
            let (mut ui, now) = panel(args);
            assert!(
                key(
                    &mut ui,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    now
                )
                .is_empty()
            );
        }
    }
    #[test]
    fn object_and_null_key_arguments_receive_the_key() {
        for args in [json!(null), json!({"handler":"minted"})] {
            let (mut ui, now) = panel(args);
            let effects = key(
                &mut ui,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                now,
            );
            assert!(
                matches!(&effects[..], [Effect::SiteKey { action, .. }] if action.args["value"]["key"].is_string())
            );
        }
    }
}
