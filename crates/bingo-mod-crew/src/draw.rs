use crate::{
    cards::{self, Card},
    state::{Panel, Rows, Tab},
};
use bingo_sdk::*;
use serde_json::{Value, json};
pub struct Drawing(pub crate::Shared);
#[async_trait::async_trait]
impl Interceptor for Drawing {
    fn id(&self) -> &str {
        crate::ID
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher {
            prefilter: json!({"component":"Pane","requestId":crate::REQUEST}),
            ..LinkMatcher::at(catalogue::UI_RENDER)
        }
    }
    async fn chain(
        &self,
        _: &catalogue::Entry,
        cx: &HookContext,
        value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let Some(mut panel) = self
            .0
            .lock()
            .map_err(crate::error)?
            .get(&cx.session)
            .cloned()
        else {
            return next.run(value).await;
        };
        let request: render::RenderRequest = serde_json::from_value(value).map_err(crate::error)?;
        let rows = crate::state::read(&cx.host, &cx.session).await?;
        let selected = (panel.tab, panel.selected);
        crate::state::clamp(&mut panel, &rows);
        follow(&mut panel, &rows, cx).await?;
        panel.watching = crate::refresh::dependencies(&cx.session, &rows);
        let models = cx.host.catalog(CatalogKind::Models).await?.entries;
        let view = {
            let mut panels = self.0.lock().map_err(crate::error)?;
            let Some(current) = panels.get_mut(&cx.session) else {
                return Ok(json!(View::text("")));
            };
            current.watching = panel.watching;
            if (current.tab, current.selected) == selected {
                current.follow = panel.follow;
            }
            let mut display = current.clone();
            crate::state::clamp(&mut display, &rows);
            tree(&display, &rows, &models, request.viewport.rows)
        };
        serde_json::to_value(view).map_err(crate::error)
    }
}
async fn follow(panel: &mut Panel, rows: &Rows, cx: &HookContext) -> Result<(), KernelError> {
    let id = if panel.tab == Tab::Agents {
        rows.agents
            .as_ref()
            .and_then(|r| r.get(panel.selected))
            .map(|r| r.summary.id.clone())
    } else {
        None
    };
    let current = panel
        .follow
        .as_ref()
        .and_then(|f| f.state.lock().ok().map(|s| s.summary.id.clone()));
    if id != current {
        panel.follow = None;
        if let Some(id) = id {
            panel.follow = Some(crate::live::follow(&cx.host, &cx.session, &id).await?);
        }
    }
    Ok(())
}
pub fn tree(panel: &Panel, rows: &Rows, models: &[CatalogEntry], height: u16) -> View {
    let header = header(panel, rows);
    let mut lines = vec![format!("crew · {header}")];
    let cards = cards(panel, rows, models);
    if cards.is_empty() {
        lines.push(empty(panel.tab).into());
    }
    let expanded = cards
        .get(panel.selected)
        .map_or(0, |card| card.details.len());
    let visible = usize::from(height).saturating_sub(expanded + 2).max(1);
    let start = panel.selected.saturating_sub(visible.saturating_sub(1));
    for (index, card) in cards.iter().enumerate().skip(start).take(visible) {
        lines.push(format!(
            "{} {}",
            if index == panel.selected { "❯" } else { " " },
            card.line
        ));
        if index == panel.selected {
            for line in &card.details {
                lines.push(format!(
                    "  {}{line}",
                    if panel.model && line.starts_with("model") {
                        "❯ "
                    } else {
                        "  "
                    }
                ));
            }
        }
    }
    if let Some(room) = &panel.confirm {
        lines.push(room.prompt());
    }
    View::Box {
        children: lines
            .into_iter()
            .map(|line| View::Text {
                children: vec![view::TextChild::String(line)],
                style: view::TextStyle {
                    wrap: Some("truncate".into()),
                    ..Default::default()
                },
            })
            .collect(),
        layout: Box::new(view::BoxStyle {
            hints: Some(
                component::SiteHints::new(if panel.confirm.is_some() {
                    vec![
                        component::KeyPair::new("⏎", "confirm"),
                        component::KeyPair::new("esc", "cancel"),
                    ]
                } else {
                    hint(panel.tab)
                })
                .composer("/crew", ""),
            ),
            on_key: Some(Action {
                name: CommandSpec::ui_name(crate::ID),
                args: json!({"handler":crate::REQUEST,"event":"press","row":rows.identity(panel.tab,panel.selected),"tab":panel.tab.label()}),
            }),
            ..Default::default()
        }),
    }
}
fn empty(tab: Tab) -> &'static str {
    match tab {
        Tab::Agents => "No agents here yet.",
        Tab::Rooms => "No rooms here yet.",
        Tab::Tasks => "No tasks here yet.",
        Tab::Team => "This project declares no team roles.",
    }
}
fn header(panel: &Panel, rows: &Rows) -> String {
    rows.tabs()
        .iter()
        .map(|tab| {
            format!(
                "{}{} ({})",
                if *tab == panel.tab { "❯ " } else { "" },
                tab.label(),
                rows.len(*tab)
            )
        })
        .collect::<Vec<_>>()
        .join(" │ ")
}

fn cards(panel: &Panel, rows: &Rows, models: &[CatalogEntry]) -> Vec<Card> {
    match panel.tab {
        Tab::Agents => rows
            .agents
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(at, row)| {
                let live = panel
                    .follow
                    .as_ref()
                    .filter(|_| at == panel.selected)
                    .and_then(|f| f.state.lock().ok());
                let price = models
                    .iter()
                    .find(|m| {
                        m.id == format!(
                            "{}/{}",
                            row.summary.provider.as_deref().unwrap_or_default(),
                            row.summary.model.as_deref().unwrap_or_default()
                        )
                    })
                    .and_then(|m| serde_json::from_value(m.meta["price"].clone()).ok());
                cards::agent_card(row, live.as_deref(), price)
            })
            .collect(),
        Tab::Rooms => rows
            .rooms
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(cards::room_card)
            .collect(),
        Tab::Tasks => rows
            .tasks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(cards::task_card)
            .collect(),
        Tab::Team => rows
            .team
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(cards::team_card)
            .collect(),
    }
}
fn hint(tab: Tab) -> Vec<component::KeyPair> {
    use component::KeyPair as Pair;
    let mut pairs = vec![Pair::new("]", "switch"), Pair::new("↑↓", "pick")];
    pairs.extend(match tab {
        Tab::Agents => vec![
            Pair::new("⏎", "step in"),
            Pair::new("s", "stop"),
            Pair::new("x", "dismiss"),
            Pair::new("m", "message"),
            Pair::new("←→", "model"),
        ],
        Tab::Rooms => vec![
            Pair::new("⏎", "open"),
            Pair::new("p", "purpose"),
            Pair::new("a", "seat"),
            Pair::new("u", "unseat"),
            Pair::new("c", "close"),
        ],
        Tab::Tasks => vec![
            Pair::new("+", "add"),
            Pair::new("space", "done/open"),
            Pair::new("o", "owner"),
        ],
        Tab::Team => vec![],
    });
    pairs.push(Pair::new("esc", "close"));
    pairs
}

#[cfg(test)]
mod chrome_tests {
    use super::*;
    #[test]
    fn all_tabs_keep_hints_out_of_the_body() {
        for tab in Tab::ALL {
            let panel = Panel {
                tab,
                ..Default::default()
            };
            bingo_sdk::testing::assert_separate_hints(&tree(&panel, &Rows::default(), &[], 20));
        }
    }
}
