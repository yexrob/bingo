//! `/think <level|off>`: the reasoning effort the next turn asks for, and the
//! next start — remembered in the user settings layer as `/model` is.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::{Change, Host};
use crate::models;
use crate::turn::ModelChoice;

pub(super) struct ThinkCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for ThinkCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "think",
            &levels(),
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
                "{}\nusage: /think {}",
                said(level, running_on(host, cx).await.as_ref()),
                levels()
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
    let choice = host
        .reconfigure(&cx.session, Change::Thinking(level))
        .await?;
    let mut message = said(level, Some(&choice));
    if let Some(refused) = super::remember(host, &[("thinking", serde_json::json!(level))]) {
        message.push('\n');
        message.push_str(&refused);
    }
    Ok(CommandOutcome::Applied {
        message: Some(message),
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
    let set = format!("thinking: {}", name(level));
    let Some(choice) = on.filter(|choice| level.is_some() && choice.reasoning.is_none()) else {
        return set;
    };
    let key = models::declared::key(choice.provider.id(), &choice.id);
    format!(
        "{set} — but {key} does not declare reasoning, so no turn asks for \
         it; models.\"{key}\".reasoning = true in settings says otherwise"
    )
}

/// The ladder as a person says it, from the sdk's one list of levels.
fn levels() -> String {
    let names: Vec<&str> = Effort::ALL.iter().map(|e| e.name()).collect();
    format!("<{}|off>", names.join("|"))
}

/// `Some(None)` is off; `None` is not a level.
fn parse(text: &str) -> Option<Option<Effort>> {
    if text.eq_ignore_ascii_case("off") {
        return Some(None);
    }
    Effort::parse(text).map(Some)
}

fn name(level: Option<Effort>) -> &'static str {
    level.map_or("off", Effort::name)
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
