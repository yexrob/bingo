use crate::{ID, REQUEST, Shared};
use bingo_sdk::*;
use serde_json::{Value, json};
pub struct Open(pub Shared, pub &'static str);
#[async_trait::async_trait]
impl Interceptor for Open {
    fn id(&self) -> &str {
        match self.1 {
            "wake" => "wake.open",
            _ => "schedules.open",
        }
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher {
            prefilter: json!({"command":self.1,"args":""}),
            ..LinkMatcher::at(catalogue::COMMAND_RUN)
        }
    }
    async fn chain(
        &self,
        _: &catalogue::Entry,
        cx: &HookContext,
        value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let input: catalogue::CommandRun =
            serde_json::from_value(value.clone()).map_err(crate::error)?;
        if input.name != self.1
            || !input.args.trim().is_empty()
            || !matches!(
                input.surface,
                Some(render::SurfaceName::Terminal | render::SurfaceName::Desktop)
            )
            || bingo_services::schedule::find(&cx.host).is_none()
        {
            return next.run(value).await;
        }
        let plain = next.run(value).await?;
        let fallback = serde_json::from_value::<CommandOutcome>(plain)
            .ok()
            .and_then(|outcome| match outcome {
                CommandOutcome::View { view } => Some(view.fold_markdown()),
                _ => None,
            });
        let cursor = crate::Cursor {
            selected: (self.1 == "wake").then_some(crate::rows::Selection::Wake),
            ..Default::default()
        };
        self.0
            .lock()
            .map_err(crate::error)?
            .insert(cx.session.clone(), cursor);
        cx.host.signal(&cx.session, ID, REQUEST, slot()).await?;
        cx.host
            .invalidate(&cx.session, vec![REQUEST.into()])
            .await?;
        serde_json::to_value(CommandOutcome::Surface {
            action: Action {
                name: "Pane".into(),
                args: json!({"plugin":ID,"id":REQUEST}),
            },
            fallback,
        })
        .map_err(crate::error)
    }
}
fn slot() -> Value {
    let mut size = slot::PaneSize::from_rows(20);
    size.columns.ideal = slot::Length::Percent(100);
    slot::with(
        json!({}),
        [slot::Slot::Pane {
            hints: None,
            id: REQUEST.into(),
            title: String::new(),
            focus: true,
            close_on_escape: false,
            hold_toasts: false,
            asked: true,
            place: vec![slot::Place::Overlay],
            size,
            view: slot::Shown::Pull {
                request: render::PullRequest {
                    component: render::Component::Pane,
                    request_id: REQUEST.into(),
                    props: json!({"owner":ID}),
                    viewport: render::Viewport {
                        columns: 0,
                        rows: 0,
                        place: None,
                    },
                },
            },
        }],
    )
}
pub async fn close(cx: &CommandContext, shared: &Shared) -> Result<(), KernelError> {
    shared.lock().map_err(crate::error)?.remove(&cx.session);
    cx.host.signal(&cx.session, ID, REQUEST, Value::Null).await
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
