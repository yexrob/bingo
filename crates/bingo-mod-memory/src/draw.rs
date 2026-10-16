use crate::{
    Shared,
    rows::{Group, Target},
};
use bingo_sdk::*;
use serde_json::{Value, json};

pub(crate) struct Drawing(pub Shared);
#[async_trait::async_trait]
impl Interceptor for Drawing {
    fn id(&self) -> &str {
        crate::ID
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher {
            prefilter: json!({"component":"Pane","props":{"owner":crate::ID}}),
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
        let request: render::RenderRequest =
            serde_json::from_value(value.clone()).map_err(crate::error)?;
        if request.props["owner"] != crate::ID {
            return next.run(value).await;
        }
        let shared = self.0.lock().map_err(crate::error)?;
        let Some(cursor) = shared.get(&cx.session) else {
            return serde_json::to_value(dim("")).map_err(crate::error);
        };
        serde_json::to_value(tree(
            &cursor.groups,
            cursor.selected,
            cursor.confirming.as_ref(),
            request.viewport.rows,
            request.viewport.columns,
        ))
        .map_err(crate::error)
    }
}
pub fn tree(
    groups: &[Group],
    selected: usize,
    confirming: Option<&Target>,
    height: u16,
    width: u16,
) -> View {
    let (lines, at) = group_lines(groups, selected);
    let room = usize::from(height).saturating_sub(4).max(1);
    let start = at
        .saturating_sub(room / 2)
        .min(lines.len().saturating_sub(room));
    let mut children = vec![pair(
        format!(
            " memory · {} tokens carried",
            crate::rows::about(crate::rows::total(groups))
        ),
        String::new(),
    )];
    children.extend(lines.into_iter().skip(start).take(room));
    children.push(dim(format!(
        " {}",
        "─".repeat(usize::from(width).saturating_sub(1))
    )));
    children.push(dim(match confirming {
        Some(Target::Memory { name, .. }) => {
            format!(" forget \"{name}\"? the file and its index line go.")
        }
        Some(target) => format!(" forget \"{}\"? the entry goes.", target.name()),
        None => String::new(),
    }));
    let target = groups
        .iter()
        .flat_map(|g| &g.rows)
        .nth(selected)
        .map(|r| &r.target);
    View::Box {
        children,
        layout: Box::new(view::BoxStyle {
            hints: Some(
                if confirming.is_some() {
                    component::SiteHints::new([
                        component::KeyPair::new("⏎", "yes"),
                        component::KeyPair::new("esc", "cancel"),
                    ])
                } else {
                    component::SiteHints::new([
                        component::KeyPair::new("↑↓", "pick"),
                        component::KeyPair::new("⏎", "read"),
                        component::KeyPair::new("d", "forget"),
                        component::KeyPair::new("esc", "close"),
                    ])
                }
                .composer("/memory", ""),
            ),
            on_key: Some(Action {
                name: CommandSpec::ui_name(crate::ID),
                args: json!({"handler":crate::REQUEST,"event":"press","target":target,"confirming":confirming}),
            }),
            ..Default::default()
        }),
    }
}
fn group_lines(groups: &[Group], selected: usize) -> (Vec<View>, usize) {
    let mut lines = Vec::new();
    let mut index = 0;
    let mut at = 0;
    for group in groups {
        if group.rows.is_empty() && index == selected {
            at = lines.len();
        }
        lines.push(dim(format!(" {} ({})", group.name, group.rows.len())));
        for row in &group.rows {
            if index == selected {
                at = lines.len();
            }
            lines.push(pair(
                format!(
                    "{}{}",
                    if index == selected { "❯ " } else { "  " },
                    row.label
                ),
                crate::rows::about(row.tokens),
            ));
            index += 1;
        }
    }
    (lines, at)
}
fn pair(label: String, value: String) -> View {
    View::Box {
        children: vec![
            View::Box {
                children: vec![View::Text {
                    children: vec![view::TextChild::String(label)],
                    style: view::TextStyle {
                        ..Default::default()
                    },
                }],
                layout: Box::new(view::BoxStyle {
                    flex_grow: Some(1),
                    width: Some(0),
                    ..Default::default()
                }),
            },
            dim(value),
        ],
        layout: Box::new(view::BoxStyle {
            flex_direction: Some(view::Direction::Row),
            gap: Some(1),
            justify_content: Some(view::Justify::SpaceBetween),
            height: Some(1),
            ..Default::default()
        }),
    }
}
fn dim(text: impl Into<String>) -> View {
    View::Text {
        children: vec![view::TextChild::String(text.into())],
        style: view::TextStyle {
            dim_color: Some(true),
            ..Default::default()
        },
    }
}
