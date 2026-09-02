//! `/think <level|off>`: the reasoning effort the next turn asks for.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::{Change, Host};
use crate::models;
use crate::turn::ModelChoice;

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
        match args.trim() {
            "" => report(&host, cx).await,
            wanted => set(&host, cx, wanted).await,
        }
    }
}

/// Bare `/think`: the level as it stands, what the model does with it, and how
/// to say one.
async fn report(host: &Arc<Host>, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let level = host.session_thinking(&cx.session)?;
    Ok(CommandOutcome::View {
        view: View::Text {
            text: format!(
                "{}\nusage: /think <{}|off>",
                said(level, running_on(host, cx).await.as_ref()),
                LEVELS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("|")
            ),
        },
    })
}

/// `/think <level|off>`: the next turn asks for this much.
async fn set(
    host: &Arc<Host>,
    cx: &CommandContext,
    wanted: &str,
) -> Result<CommandOutcome, KernelError> {
    let level = parse(wanted).ok_or_else(|| {
        KernelError::new(
            ErrorCode::InvalidInput,
            format!("unknown thinking level: {wanted}"),
        )
    })?;
    host.reconfigure(&cx.session, Change::Thinking(level))
        .await?;
    Ok(CommandOutcome::Applied {
        message: Some(said(level, running_on(host, cx).await.as_ref())),
    })
}

/// The model the session's next turn would run on, where one can be resolved.
/// It is read to tell the truth about the level and for nothing else, so a
/// session that cannot answer — a log session, a provider that is not signed
/// in — reports the level alone rather than refusing to report at all.
async fn running_on(host: &Arc<Host>, cx: &CommandContext) -> Option<ModelChoice> {
    host.session_model(&cx.session).await.ok().flatten()
}

/// What the level means. A model that does not declare reasoning is sent no
/// reasoning parameter at all — `ModelChoice::reasoning` is `None` however
/// high the level — so the level alone would read as a promise no turn keeps.
/// The level is still kept, and takes effect the moment `/model` moves to a
/// model that reasons.
fn said(level: Option<Effort>, on: Option<&ModelChoice>) -> String {
    let said = format!("thinking: {}", name(level));
    let Some(choice) = on.filter(|choice| level.is_some() && choice.reasoning.is_none()) else {
        return said;
    };
    let key = models::declared::key(choice.provider.id(), &choice.id);
    format!(
        "{said} — but {key} does not declare reasoning, so no turn asks for \
         it; models.\"{key}\".reasoning = true in settings says otherwise"
    )
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
