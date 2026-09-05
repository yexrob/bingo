//! `/think <level|off>`: the reasoning effort the next turn asks for, and the
//! next start — remembered in the user settings layer as `/model` is.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::Host;
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
    let level = Effort::spoken(wanted).ok_or_else(|| {
        KernelError::new(
            ErrorCode::InvalidInput,
            format!("unknown thinking level: {wanted}"),
        )
    })?;
    host.reconfigure(&cx.session, SessionChange::Thinking(level))
        .await?;
    let mut message = said(level, running_on(host, cx).await.as_ref());
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
    let set = format!("thinking: {}", Effort::word(level));
    let Some(choice) = on.filter(|choice| level.is_some() && choice.reasoning.is_none()) else {
        return set;
    };
    let key = models::declared::key(choice.provider.id(), &choice.id);
    format!(
        "{set} — but {key} does not declare reasoning, so no turn asks for \
         it; models.\"{key}\".reasoning = true in settings says otherwise"
    )
}

/// The ladder as a person says it, from the sdk's one list of words.
fn levels() -> String {
    format!("<{}>", Effort::words().collect::<Vec<_>>().join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The usage line is the sdk's list and nothing kept beside it, so a
    /// level added there is offered here without a second edit.
    #[test]
    fn the_usage_line_lists_every_word_a_level_may_be_said_in() {
        assert_eq!(levels(), "<minimal|low|medium|high|xhigh|max|off>");
    }
}
