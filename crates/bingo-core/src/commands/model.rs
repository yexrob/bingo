//! `/model` remembers root choices for next startup; `--keep` writes settings too.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use serde_json::json;

use crate::host::{Host, model_switch};
use crate::turn::ModelChoice;

const USAGE: &str = "usage: /model [<provider>/]<model>";

pub(super) struct ModelCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for ModelCommand {
    fn now(&self, view: &command::SessionView<'_>) -> Option<String> {
        let state = view.state;
        state.summary.model.as_ref().map(|model| {
            state
                .summary
                .provider
                .as_ref()
                .map(|provider| format!("{provider}/{model}"))
                .unwrap_or_else(|| model.clone())
        })
    }

    fn spec(&self) -> CommandSpec {
        let mut spec = super::spec(
            "model",
            "model, thinking and providers",
            super::keep_args(ArgSpec::Catalog {
                source: "models".into(),
            }),
            true,
        );
        spec.keys = vec![super::next_key("alt+m")];
        spec
    }

    async fn is_instant(&self, args: &str, state: &SessionState) -> bool {
        if args.trim().is_empty() || super::pin::parse(args).is_some() {
            return true;
        }
        let Ok(host) = super::host(&self.host) else {
            return true;
        };
        let Ok((wanted, _)) = super::choice_args(args) else {
            return true;
        };
        match target(&host, &state.summary.id, wanted).await {
            Ok(choice) => !crate::host::model_switch::needs_compaction(state, &choice),
            Err(_) => true,
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        if let Some((pin, id)) = super::pin::parse(args) {
            return super::pin::run(&host, cx, id, pin).await;
        }
        match args.trim() {
            "" => report(&host, cx).await,
            wanted => set(&host, cx, wanted).await,
        }
    }
}

/// Bare `/model`: what the session runs on — who serves it, which model, and
/// how hard it is asked to think — read from the resolved choice, which is
/// what the next turn will actually ask. Then the pins and how many models
/// there are; `/models` is the listing.
async fn report(host: &Arc<Host>, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let choice = running_on(host, cx).await?;
    let listed = host.catalog(CatalogKind::Models).await?.entries.len();
    Ok(CommandOutcome::View {
        view: View::text(format!(
            "{}\n{USAGE}\n{}",
            said(&choice),
            landscape(&crate::host::pins::pinned_models(host), listed)
        )),
    })
}

/// The pins in their order and a count, never every id a provider serves.
fn landscape(pins: &[String], listed: usize) -> String {
    let pinned = if pins.is_empty() {
        "none · /model pin <id> adds one".to_owned()
    } else {
        pins.join(", ")
    };
    let models = if listed == 1 { "model" } else { "models" };
    format!("pinned: {pinned}\n{listed} {models} · /models lists them")
}

async fn target(
    host: &Host,
    session: &SessionId,
    wanted: &str,
) -> Result<ModelChoice, KernelError> {
    let next;
    let wanted = if wanted == "next" {
        next = next_pin(host, session).await?;
        &next
    } else {
        wanted
    };
    let known = host.catalog(CatalogKind::Providers).await?.entries;
    let (provider, model) = split(wanted, |id| known.iter().any(|p| p.id == id))?;
    model_switch::proposed_model(host, session, provider.map(str::to_owned), model.to_owned()).await
}

/// The pin after the session's model, in the order the `pinned` setting
/// keeps (ADR-0085 §4), skipping pins no provider offers any more.
async fn next_pin(host: &Host, session: &SessionId) -> Result<String, KernelError> {
    let choice = host
        .session_model(session)
        .await?
        .ok_or_else(|| KernelError::new(ErrorCode::InvalidInput, "a log session has no model"))?;
    let current = format!("{}/{}", choice.endpoint.id(), choice.id);
    let listed = host.catalog(CatalogKind::Models).await?.entries;
    let pins = crate::host::pins::pinned_models(host)
        .into_iter()
        .filter(|pin| listed.iter().any(|entry| &entry.id == pin))
        .collect::<Vec<_>>();
    super::next::after(&pins, &current)
        .map(str::to_owned)
        .ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                "No pinned models; use /model pin <id> first.",
            )
        })
}

async fn set(
    host: &Arc<Host>,
    cx: &CommandContext,
    args: &str,
) -> Result<CommandOutcome, KernelError> {
    let (wanted, keep) = super::choice_args(args)?;
    let choice = target(host, &cx.session, wanted).await?;
    let pictures = !choice.capabilities.images
        && crate::host::model_switch::has_pictures(&host.session_state(&cx.session).await?);
    let compacted = model_switch::compact_for_switch(host, &cx.session, &choice).await?;
    if cx.cancel.is_cancelled() {
        return Err(KernelError::new(
            ErrorCode::Interrupted,
            "model switch cancelled",
        ));
    }
    let saved = apply_choice(host, cx, &choice, keep).await?;
    let mut message = format!(
        "⇄ {} · {} · thinking {} · {}",
        choice.id,
        choice.endpoint.id(),
        Effort::word(choice.reasoning),
        saved
    );
    if let Some((before, after)) = compacted {
        message.push_str(&format!(" · compacted {before} → {after}"));
    }
    if pictures {
        message.push_str(&format!(" · pictures stay, {} won't see them", choice.id));
    }
    Ok(CommandOutcome::Applied {
        message: Some(message),
    })
}

async fn apply_choice(
    host: &Host,
    cx: &CommandContext,
    choice: &ModelChoice,
    keep: bool,
) -> Result<String, KernelError> {
    let change = |scope| SessionChange::Model {
        provider: Some(choice.endpoint.id().to_owned()),
        model: choice.id.clone(),
        scope,
    };
    host.reconfigure(&cx.session, change(ChoiceScope::Session))
        .await?;
    if !keep {
        return crate::host::remembered::remember_model(host, &cx.session, false).await;
    }
    let keys = [
        ("provider", json!(choice.endpoint.id())),
        ("model", json!(choice.id)),
    ];
    let path = crate::settings::user_path(host.env());
    if let Err(error) = crate::settings::remember(&path, &keys) {
        let remembered = crate::host::remembered::remember_model(host, &cx.session, false).await?;
        return Ok(format!("{remembered} · user settings not kept: {error}"));
    }
    host.reconfigure(&cx.session, change(ChoiceScope::Default))
        .await?;
    crate::host::remembered::remember_model(host, &cx.session, true).await
}

/// What the session's next turn runs on, resolved as the turn will resolve
/// it. A log session has none (ADR-0011 §1) and is told so.
async fn running_on(host: &Arc<Host>, cx: &CommandContext) -> Result<ModelChoice, KernelError> {
    host.session_model(&cx.session)
        .await?
        .ok_or_else(|| KernelError::new(ErrorCode::InvalidInput, "a log session has no model"))
}

/// The session's model and requested effort, in the vocabulary `/think` uses.
fn said(choice: &ModelChoice) -> String {
    let mut text = format!("model: {}/{}", choice.endpoint.id(), choice.id);
    if let Some(effort) = choice.reasoning {
        text.push_str(&format!("\nthinking: {}", effort.name()));
    }
    text
}

/// `provider/model` when the first segment names a provider; else the whole
/// text is the model and the provider stays. A path with an empty segment
/// names no model.
fn split(
    text: &str,
    is_provider: impl Fn(&str) -> bool,
) -> Result<(Option<&str>, &str), KernelError> {
    if text.split('/').any(str::is_empty) {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("`{text}` names no model; use /model <provider>/<model>"),
        ));
    }
    Ok(match text.split_once('/') {
        Some((provider, model)) if is_provider(provider) => (Some(provider), model),
        _ => (None, text),
    })
}

#[cfg(test)]
mod tests {
    use super::{landscape, split};
    use bingo_sdk::ErrorCode;

    #[test]
    fn the_report_names_pins_and_counts_the_rest() {
        assert_eq!(
            landscape(&[], 1),
            "pinned: none · /model pin <id> adds one\n1 model · /models lists them"
        );
        assert_eq!(
            landscape(&["b/x".into(), "a/y".into()], 40),
            "pinned: b/x, a/y\n40 models · /models lists them"
        );
    }

    #[test]
    fn a_leading_provider_is_taken_only_when_it_is_one() {
        let known = |id: &str| id == "openai";
        assert_eq!(
            split("openai/gpt-5", known).unwrap(),
            (Some("openai"), "gpt-5")
        );
        assert_eq!(
            split("meta/llama-3", known).unwrap(),
            (None, "meta/llama-3"),
            "an unknown first segment is part of the model id"
        );
        assert_eq!(split("gpt-5", known).unwrap(), (None, "gpt-5"));
        for empty in ["openai/", "/gpt-5", "/", "", "a//b"] {
            let error = split(empty, known).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidInput, "{empty}");
        }
    }
}
