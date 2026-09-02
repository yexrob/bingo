//! `DemoProgress`: a tool that publishes a `Progress` on a clock and returns
//! the loop it ran as its display. It is the live lane of ADR-0013 §2 in one
//! screenful: a signal costs the journal nothing, so a bar may move ten times
//! a second and leave no trace after `--continue`.

use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, KernelError, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, View,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::journal::{PLUGIN, PROGRESS};

/// How many steps the bar takes, and how long each one lasts: three seconds
/// of movement, which is long enough to watch and short enough to wait for.
const STEPS: u64 = 15;
const STEP: Duration = Duration::from_millis(200);

/// What the tool shows a person once it is done: the loop it just ran.
const LOOP: &str = r#"for step in 0..=15 {
    host.signal(session, "bingo.demo.ui", "progress", progress(step, 15)).await?;
    sleep(Duration::from_millis(200)).await;
}"#;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// What the bar is called while it runs. Defaults to the tool's own name.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DemoProgressTool;

#[async_trait]
impl Tool for DemoProgressTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "DemoProgress".into(),
            description: "Publish a live progress bar for three seconds (a demo of ADR-0013)."
                .into(),
            input_schema: input_schema::<Args>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &serde_json::Value) -> ToolTraits {
        ToolTraits {
            read_only: true,
            trusted: true,
            // One kind, one publisher: two calls at once would fight over it.
            concurrency_safe: false,
            ..ToolTraits::default()
        }
    }

    async fn call(
        &self,
        input: serde_json::Value,
        cx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: Args =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let label = args.label.unwrap_or_else(|| "DemoProgress".into());
        for step in 0..=STEPS {
            publish(cx, step, &label).await?;
            if step < STEPS {
                tokio::time::sleep(STEP).await;
            }
        }
        Ok(done())
    }
}

/// One frame of the bar. The reducer keeps the latest per kind, so publishing
/// the whole of it every step costs one value, not a history.
async fn publish(cx: &ToolContext, step: u64, label: &str) -> Result<(), ToolError> {
    let payload = serde_json::to_value(View::Progress {
        value: step,
        total: Some(STEPS),
        label: Some(label.to_string()),
    })
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    cx.host
        .signal(&cx.session, PLUGIN, PROGRESS, payload)
        .await
        .map_err(failed)
}

fn failed(error: KernelError) -> ToolError {
    ToolError::Failed(error.message)
}

/// What the model reads, and what a person reads beside it.
fn done() -> ToolOutput {
    ToolOutput {
        parts: vec![ContentPart::text(format!(
            "Published {} progress frames over 3s.",
            STEPS + 1
        ))],
        is_error: false,
        display: Some(View::Code {
            lang: Some("rust".into()),
            text: LOOP.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Journals, tool_context};

    /// Time is paused, so the three seconds cost the test nothing.
    #[tokio::test(start_paused = true)]
    async fn the_bar_is_published_once_a_step_and_never_journaled() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);

        let out = DemoProgressTool
            .call(serde_json::json!({"label": "cargo test"}), &cx)
            .await
            .expect("a run");

        let signals = journals.signals();
        assert_eq!(
            signals.len() as u64,
            STEPS + 1,
            "one per step, and the last"
        );
        assert!(signals.iter().all(|(kind, _)| kind == PROGRESS));
        assert_eq!(
            signals[0].1,
            serde_json::json!({
                "kind": "progress", "value": 0, "total": STEPS, "label": "cargo test"
            })
        );
        assert_eq!(
            signals[STEPS as usize].1["value"],
            serde_json::json!(STEPS),
            "the last frame is the full bar"
        );
        assert!(
            journals.extensions().is_empty(),
            "a live bar writes nothing into the journal"
        );
        assert!(matches!(out.display, Some(View::Code { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn the_label_is_the_tools_own_name_when_none_is_given() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        DemoProgressTool
            .call(serde_json::json!({}), &cx)
            .await
            .expect("a run");
        assert_eq!(
            journals.signals()[0].1["label"],
            serde_json::json!("DemoProgress")
        );
    }

    #[test]
    fn the_tool_reads_nothing_and_is_alone_in_its_kind() {
        let traits = DemoProgressTool.traits(&serde_json::Value::Null);
        assert!(traits.read_only && traits.trusted);
        assert!(!traits.concurrency_safe);
    }
}
