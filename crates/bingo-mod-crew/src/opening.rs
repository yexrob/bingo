use crate::state::Tab;
use bingo_sdk::*;
use serde_json::{Value, json};

pub const FOLDED: [(&str, Tab); 5] = [
    ("agents", Tab::Agents),
    ("tasks", Tab::Tasks),
    ("room", Tab::Rooms),
    ("rooms", Tab::Rooms),
    ("team", Tab::Team),
];

pub struct Opening {
    pub shared: crate::Shared,
    pub name: &'static str,
    pub tab: Tab,
}

#[async_trait::async_trait]
impl Interceptor for Opening {
    fn id(&self) -> &str {
        self.name
    }

    fn matcher(&self) -> LinkMatcher {
        LinkMatcher {
            prefilter: json!({"command":self.name,"args":""}),
            ..LinkMatcher::at(catalogue::COMMAND_RUN)
        }
    }

    async fn chain(
        &self,
        _: &catalogue::Entry,
        cx: &HookContext,
        input: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let run: catalogue::CommandRun =
            serde_json::from_value(input.clone()).map_err(crate::error)?;
        if run.name != self.name
            || !run.args.trim().is_empty()
            || !matches!(
                run.surface,
                Some(render::SurfaceName::Terminal | render::SurfaceName::Desktop)
            )
        {
            return next.run(input).await;
        }
        let plain = next.run(input).await?;
        let fallback = match serde_json::from_value(plain.clone()) {
            Ok(CommandOutcome::View { view }) => Some(view.fold_markdown()),
            Ok(CommandOutcome::Applied { message }) => message,
            _ => return Ok(plain),
        };
        let outcome =
            crate::command::open(&self.shared, &cx.host, &cx.session, self.tab, fallback).await?;
        serde_json::to_value(outcome).map_err(crate::error)
    }
}
