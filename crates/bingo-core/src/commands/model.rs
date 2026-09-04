//! `/model [<provider>/]<model>`: the next turn runs on it, and so does the
//! next start — the choice is written into the user settings layer the way
//! Claude Code's `/model` writes its `model` key (ADR-0003 §5, amended
//! 2026-09-04). A `--model` on the command line is a layer of its own and is
//! not touched here: a one-run override stays one run.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use serde_json::json;

use crate::host::{Change, Host};
use crate::settings;
use crate::turn::ModelChoice;

const USAGE: &str = "usage: /model [<provider>/]<model>";

pub(super) struct ModelCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for ModelCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "model",
            "[<provider>/]<model>",
            ArgSpec::Catalog {
                source: "models".into(),
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

/// Bare `/model`: what the session runs on — who serves it, which model, and
/// how hard it is asked to think — read from the resolved choice, which is
/// what the next turn will actually ask.
async fn report(host: &Arc<Host>, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let choice = host
        .session_model(&cx.session)
        .await?
        .ok_or_else(|| KernelError::new(ErrorCode::InvalidInput, "a log session has no model"))?;
    Ok(CommandOutcome::View {
        view: View::Text {
            text: format!("{}\n{USAGE}", said(&choice)),
        },
    })
}

/// `/model [<provider>/]<model>`: the next turn runs on it, and it is
/// remembered for the next start.
async fn set(
    host: &Arc<Host>,
    cx: &CommandContext,
    wanted: &str,
) -> Result<CommandOutcome, KernelError> {
    let known = host.providers().await;
    let (provider, model) = split(wanted, |id| known.iter().any(|p| p.id() == id));
    let choice = host
        .reconfigure(
            &cx.session,
            Change::Model {
                provider: provider.map(str::to_string),
                model: model.to_string(),
            },
        )
        .await?;
    let mut message = said(&choice);
    if let Some(refused) = remember(host, &choice) {
        message.push('\n');
        message.push_str(&refused);
    }
    Ok(CommandOutcome::Applied {
        message: Some(message),
    })
}

/// Write the choice into the user layer, so the next start opens on it.
/// The session has already moved; a file that will not take the note says so
/// out loud rather than undoing what the person asked for. `None` is written.
fn remember(host: &Host, choice: &ModelChoice) -> Option<String> {
    let path = settings::user_path(host.env());
    let keys = [
        ("provider", json!(choice.provider.id())),
        ("model", json!(choice.id)),
    ];
    settings::remember(&path, &keys)
        .err()
        .map(|e| format!("this session has it, but the next will not: {e}"))
}

/// The three facts a person asked for, in the vocabulary `/think` uses for
/// the third. The effort is the one the turn will ask for, so a model that
/// does not reason shows none rather than a level no request carries.
fn said(choice: &ModelChoice) -> String {
    let mut text = format!("model: {}/{}", choice.provider.id(), choice.id);
    if let Some(effort) = choice.reasoning {
        text.push_str(&format!("\nthinking: {}", effort.name()));
    }
    text
}

/// `provider/model` when the first segment names a provider; else the whole
/// text is the model and the provider stays.
fn split(text: &str, is_provider: impl Fn(&str) -> bool) -> (Option<&str>, &str) {
    match text.split_once('/') {
        Some((provider, model)) if is_provider(provider) && !model.is_empty() => {
            (Some(provider), model)
        }
        _ => (None, text),
    }
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_leading_provider_is_taken_only_when_it_is_one() {
        let known = |id: &str| id == "openai";
        assert_eq!(split("openai/gpt-5", known), (Some("openai"), "gpt-5"));
        assert_eq!(
            split("meta/llama-3", known),
            (None, "meta/llama-3"),
            "an unknown first segment is part of the model id"
        );
        assert_eq!(split("gpt-5", known), (None, "gpt-5"));
        assert_eq!(split("openai/", known), (None, "openai/"));
    }
}
