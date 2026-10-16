use crate::{Confirmation, ID, REQUEST, Shared, rows};
use bingo_sdk::*;
use bingo_services::schedule::{Listing, Pending, Row};
use serde_json::{Value, json};
pub(crate) struct Drawing(pub Shared);
#[async_trait::async_trait]
impl Interceptor for Drawing {
    fn id(&self) -> &str {
        ID
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher {
            prefilter: json!({"component":"Pane","props":{"owner":ID}}),
            ..LinkMatcher::at(catalogue::UI_RENDER)
        }
    }
    async fn chain(
        &self,
        _: &catalogue::Entry,
        cx: &HookContext,
        value: Value,
        _: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let request: render::RenderRequest = serde_json::from_value(value).map_err(crate::error)?;
        let (listing, wake) = crate::listing(&cx.host, &cx.session).await?;
        let rows = rows::rows(&listing, wake.as_ref());
        let mut shared = self.0.lock().map_err(crate::error)?;
        let cursor = shared.entry(cx.session.clone()).or_default();
        cursor.seen = Some(std::time::Instant::now());
        let at = rows::selected(&rows, cursor.selected.as_ref());
        cursor.selected = rows.get(at).map(rows::Row::selection);
        if cursor
            .confirming
            .as_ref()
            .is_some_and(|r| rows.get(at).and_then(rows::Row::schedule) != Some(r.row()))
        {
            cursor.confirming = None;
        }
        serde_json::to_value(tree_with_wake(
            &listing,
            wake.as_ref(),
            at,
            cursor.confirming.as_ref(),
            request.viewport.columns,
            request.viewport.rows,
            jiff::Timestamp::now(),
        ))
        .map_err(crate::error)
    }
}
pub fn tree(
    listing: &Listing,
    selected: usize,
    confirming: Option<&Confirmation>,
    width: u16,
    height: u16,
    now: jiff::Timestamp,
) -> View {
    tree_with_wake(listing, None, selected, confirming, width, height, now)
}

pub fn tree_with_wake(
    listing: &Listing,
    wake: Option<&Pending>,
    selected: usize,
    confirming: Option<&Confirmation>,
    width: u16,
    height: u16,
    now: jiff::Timestamp,
) -> View {
    let rows = rows::rows(listing, wake);
    let selected_row = rows.get(selected);
    let mut children = vec![line(
        format!(" schedules · {}", holder(&listing.holder)),
        false,
    )];
    let (body, armed) = match confirming {
        Some(Confirmation::Confirm(row)) => crate::whole::body(row, width, height),
        _ => (entries(&rows, selected, height, now), true),
    };
    children.extend(body);
    if let Some(trouble) = &listing.trouble {
        children.push(View::Badge {
            text: trouble.clone(),
            tone: bingo_sdk::Tone::Bad,
        });
    }
    children.push(line("─".repeat(usize::from(width)), true));
    children.push(line(footer(selected_row, confirming, armed), true));
    // A confirmation not shown whole is not offered to ⏎.
    let confirming = confirming.filter(|_| armed);
    View::Box {
        children,
        layout: Box::new(view::BoxStyle {
            hints: Some(hints(selected_row, confirming.is_some()).composer("/schedule", "")),
            on_key: Some(Action {
                name: format!("{ID}:ui"),
                args: json!({"handler":REQUEST,"event":"press","row":selected_row,"confirming":confirming}),
            }),
            ..Default::default()
        }),
    }
}
fn hints(selected: Option<&rows::Row>, confirming: bool) -> component::SiteHints {
    use component::KeyPair;
    if confirming {
        return component::SiteHints::new([
            KeyPair::new("⏎", "yes"),
            KeyPair::new("esc", "cancel"),
        ]);
    }
    if matches!(selected, Some(rows::Row::Wake(_))) {
        return component::SiteHints::new([
            KeyPair::new("↑↓", "pick"),
            KeyPair::new("d", "end wake"),
            KeyPair::new("esc", "close"),
        ]);
    }
    component::SiteHints::new([
        KeyPair::new("↑↓", "pick"),
        KeyPair::new("space", "pause/enable"),
        KeyPair::new("r", "run"),
        KeyPair::new("c", "confirm"),
        KeyPair::new("d", "forget"),
        KeyPair::new("→", "session"),
        KeyPair::new("esc", "close"),
    ])
}

/// The rows that fit, scrolled to keep the selection in view.
fn entries(rows: &[rows::Row], selected: usize, height: u16, now: jiff::Timestamp) -> Vec<View> {
    if rows.is_empty() {
        return vec![line(
            " no schedules yet · /schedule add every 2h -- check".into(),
            true,
        )];
    }
    let room = usize::from(height.saturating_sub(5)).max(1);
    let start = selected
        .saturating_sub(room / 2)
        .min(rows.len().saturating_sub(room));
    rows.iter()
        .enumerate()
        .skip(start)
        .take(room)
        .map(|(index, row)| entry(row, index == selected, now))
        .collect()
}
fn entry(row: &rows::Row, selected: bool, now: jiff::Timestamp) -> View {
    match row {
        rows::Row::Schedule(row) => schedule(row, selected, now),
        rows::Row::Wake(wake) => line(
            format!(
                "{}wake  {}  {}{}",
                if selected { "❯ " } else { "  " },
                wake.at
                    .parse()
                    .map(|at| bingo_sdk::until(now, at))
                    .unwrap_or_else(|_| "—".into()),
                bingo_services::schedule::visible(&wake.note)
                    .split('\n')
                    .next()
                    .unwrap_or_default(),
                more(&wake.note),
            ),
            false,
        ),
    }
}

fn schedule(row: &Row, selected: bool, now: jiff::Timestamp) -> View {
    line(
        format!(
            "{}{}  {}  {}  {}  {}  {}{}",
            if selected { "❯ " } else { "  " },
            row.id,
            row.spec,
            next(row, now),
            if row.enabled { "on" } else { "off" },
            row.by,
            bingo_services::schedule::visible(&row.text)
                .split('\n')
                .next()
                .unwrap_or_default(),
            more(&row.text)
        ),
        !row.enabled,
    )
}
/// What a first line leaves out, said so a row never passes for the whole.
fn more(text: &str) -> String {
    match text.lines().count() {
        0 | 1 => String::new(),
        2 => "  (+1 line)".into(),
        lines => format!("  (+{} lines)", lines - 1),
    }
}
fn footer(selected: Option<&rows::Row>, confirming: Option<&Confirmation>, armed: bool) -> String {
    if let Some(confirmation) = confirming {
        let row = confirmation.row();
        if matches!(confirmation, Confirmation::Confirm(_)) {
            return confirm_footer(row, armed);
        }
        return format!(
            " forget {} \"{} {}\"{}? its session stays.",
            row.id,
            row.spec,
            bingo_services::schedule::visible(&row.text)
                .split('\n')
                .next()
                .unwrap_or_default(),
            more(&row.text)
        );
    }
    if matches!(selected, Some(rows::Row::Wake(_))) {
        return " pending wake on this session".into();
    }
    selected
        .and_then(rows::Row::schedule)
        .map(|row| {
            format!(
                " mode: {} · session: {}",
                row.mode.as_deref().unwrap_or("the session's"),
                row.session
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "has not fired yet".into())
            )
        })
        .unwrap_or_default()
}
fn confirm_footer(row: &Row, armed: bool) -> String {
    match armed {
        true => format!(
            " confirm {} as yours, mode {}?",
            row.id,
            row.mode.as_deref().unwrap_or("the session's")
        ),
        false => format!(
            " cannot confirm here: `/schedule confirm {}` answers with all of it",
            row.id
        ),
    }
}
fn next(row: &Row, now: jiff::Timestamp) -> String {
    row.next_fire
        .as_ref()
        .and_then(|at| at.parse().ok())
        .map(|at| bingo_sdk::until(now, at))
        .unwrap_or_else(|| "—".into())
}
fn holder(value: &str) -> &str {
    if value.contains("another runner") {
        "dormant — held by another bingo"
    } else if value.contains("no runner") {
        "no runner"
    } else {
        value
    }
}
pub(crate) fn line(text: String, dim: bool) -> View {
    View::Text {
        children: vec![view::TextChild::String(text)],
        style: view::TextStyle {
            dim_color: dim.then_some(true),
            wrap: Some("truncate".into()),
            ..Default::default()
        },
    }
}

#[cfg(test)]
#[path = "draw_tests.rs"]
mod tests;
