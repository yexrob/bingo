//! `/status`: what the session is right now, as one key-value view — the
//! facts the surface keeps off its screen until asked (design §7).

use std::sync::Weak;

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::Host;

pub(super) struct StatusCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for StatusCommand {
    fn spec(&self) -> CommandSpec {
        super::spec("status", "", ArgSpec::None, true)
    }

    async fn run(&self, _args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let state = super::host(&self.host)?.session_state(&cx.session).await?;
        Ok(CommandOutcome::View {
            view: View::KeyValue { rows: rows(&state) },
        })
    }
}

fn rows(state: &SessionState) -> Vec<(String, String)> {
    let s = &state.summary;
    let mut rows = vec![
        ("session".into(), s.id.to_string()),
        ("cwd".into(), s.cwd.clone()),
        (
            "provider".into(),
            s.provider.clone().unwrap_or_else(|| "?".into()),
        ),
        (
            "model".into(),
            s.model.clone().unwrap_or_else(|| "?".into()),
        ),
        ("mode".into(), mode(&state.config)),
        ("context".into(), context(state.context)),
        (
            "tokens".into(),
            format!(
                "{} in · {} out",
                s.usage.input_tokens, s.usage.output_tokens
            ),
        ),
    ];
    if let Some(parent) = &s.parent {
        rows.push(("parent".into(), parent.session.to_string()));
    }
    rows
}

fn mode(config: &ConfigView) -> String {
    config
        .plugins
        .get("bingo.permissions")
        .and_then(|policy| policy.get("mode"))
        .and_then(|mode| mode.as_str())
        .unwrap_or("default")
        .to_string()
}

fn context(context: Option<ContextUsage>) -> String {
    match context {
        Some(c) => format!(
            "{} of {} tokens ({} %) · compacts at {}",
            c.used,
            c.window,
            c.percent(),
            c.trigger
        ),
        None => "not measured yet".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_is_the_policy_s_or_default() {
        let mut config = ConfigView::default();
        assert_eq!(mode(&config), "default");
        config.plugins.insert(
            "bingo.permissions".into(),
            serde_json::json!({"mode": "acceptEdits"}),
        );
        assert_eq!(mode(&config), "acceptEdits");
    }

    #[test]
    fn the_context_row_reads_as_a_fraction() {
        assert_eq!(context(None), "not measured yet");
        let usage = ContextUsage {
            used: 42_000,
            window: 100_000,
            trigger: 80_000,
        };
        assert_eq!(
            context(Some(usage)),
            "42000 of 100000 tokens (42 %) · compacts at 80000"
        );
    }
}
