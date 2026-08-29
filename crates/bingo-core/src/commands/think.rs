//! `/think <level|off>`: the reasoning effort the next turn asks for.

use std::sync::Weak;

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::{Change, Host};

const LEVELS: &[(&str, Effort)] = &[
    ("minimal", Effort::Minimal),
    ("low", Effort::Low),
    ("medium", Effort::Medium),
    ("high", Effort::High),
    ("xhigh", Effort::XHigh),
    ("max", Effort::Max),
];

pub(super) struct ThinkCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for ThinkCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "think",
            "<minimal|low|medium|high|xhigh|max|off>",
            ArgSpec::Free {
                hint: "level, or off".into(),
            },
            true,
        )
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        let wanted = args.trim();
        if wanted.is_empty() {
            let current = host.session_thinking(&cx.session)?;
            return Ok(CommandOutcome::View {
                view: View::Text {
                    text: format!(
                        "thinking: {}\nusage: /think <{}|off>",
                        name(current),
                        LEVELS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("|")
                    ),
                },
            });
        }
        let level = parse(wanted).ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                format!("unknown thinking level: {wanted}"),
            )
        })?;
        host.reconfigure(&cx.session, Change::Thinking(level))
            .await?;
        Ok(CommandOutcome::Applied {
            message: Some(format!("thinking: {}", name(level))),
        })
    }
}

/// `Some(None)` is off; `None` is not a level.
fn parse(text: &str) -> Option<Option<Effort>> {
    let lower = text.to_ascii_lowercase();
    if lower == "off" {
        return Some(None);
    }
    LEVELS
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, e)| Some(*e))
}

fn name(level: Option<Effort>) -> &'static str {
    level
        .and_then(|l| LEVELS.iter().find(|(_, e)| *e == l))
        .map_or("off", |(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_case_insensitively_and_off_is_none() {
        assert_eq!(parse("XHigh"), Some(Some(Effort::XHigh)));
        assert_eq!(parse("off"), Some(None));
        assert_eq!(parse("loud"), None);
        assert_eq!(name(Some(Effort::Max)), "max");
        assert_eq!(name(None), "off");
    }
}
