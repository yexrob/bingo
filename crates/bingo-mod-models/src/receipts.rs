//! Terminal switches acknowledge in the footer; text clients retain their receipt.
use bingo_sdk::*;
use serde_json::Value;
pub struct Receipts(pub crate::Shared);
#[async_trait::async_trait]
impl Interceptor for Receipts {
    fn id(&self) -> &str {
        "models.receipts"
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher::at(catalogue::COMMAND_RUN)
    }
    async fn chain(
        &self,
        _: &catalogue::Entry,
        cx: &HookContext,
        value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let run: catalogue::CommandRun =
            serde_json::from_value(value.clone()).map_err(crate::error)?;
        let sheets = run.surface.is_some_and(render::SurfaceName::shows_sheets);
        let switches = matches!(run.name.as_str(), "model" | "think");
        let command = run.name;
        let value = next.run(value).await?;
        let Some(record) = switches.then(|| receipt(&value)).flatten() else {
            return Ok(value);
        };
        let choosing = self
            .0
            .lock()
            .map_err(crate::error)?
            .contains_key(&cx.session);
        if !sheets || !choosing {
            crate::toast::announce(cx, &command).await?;
        }
        if sheets && !persistence_warning(&value) {
            return Ok(serde_json::json!({"kind":"applied"}));
        }
        serde_json::to_value(record).map_err(crate::error)
    }
}
fn persistence_warning(value: &Value) -> bool {
    value["message"].as_str().is_some_and(|message| {
        message.contains("could not remember next-start default")
            || message.contains("user settings not kept")
    })
}

/// The transcript line a switch leaves; anything else the command said is not one.
fn receipt(value: &Value) -> Option<CommandOutcome> {
    if value["kind"] != "applied" {
        return None;
    }
    let message = value["message"].as_str()?;
    let text = if message.starts_with('⇄') {
        message.to_owned()
    } else {
        format!("⇄ thinking {}", message.strip_prefix("thinking: ")?)
    };
    Some(CommandOutcome::Record {
        body: Box::new(ItemBody::Notice {
            level: Level::Info,
            code: "model.choice".into(),
            text,
        }),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn choice_receipts_are_recorded_and_other_outcomes_are_unchanged() {
        let value = receipt(&json!({"kind":"applied","message":"⇄ small · here"}));
        assert!(
            matches!(value, Some(CommandOutcome::Record { body }) if matches!(body.as_ref(), ItemBody::Notice {text, ..} if text == "⇄ small · here"))
        );
        assert!(receipt(&json!({"kind":"applied","message":"pinned"})).is_none());
    }
    #[tokio::test]
    async fn neither_sheets_nor_text_clients_hide_failed_model_persistence() {
        for (surface, choosing) in [("terminal", true), ("terminal", false), ("text", false)] {
            for warning in [
                "could not remember next-start default: locked",
                "user settings not kept: read-only",
            ] {
                let outcome = warning_receipt(surface, choosing, warning).await;
                assert!(
                    matches!(outcome, CommandOutcome::Record { body } if matches!(body.as_ref(), ItemBody::Notice {text, ..} if text.contains(warning)))
                );
            }
        }
    }

    async fn warning_receipt(surface: &str, choosing: bool, warning: &str) -> CommandOutcome {
        let (host, command) = crate::test_support::host();
        host.on_catalog(|kind| async move {
            Ok(Catalog {
                kind,
                entries: vec![],
            })
        });
        let shared = crate::Shared::default();
        if choosing {
            shared
                .lock()
                .unwrap()
                .insert(command.session.clone(), Default::default());
        }
        let cx = HookContext {
            session: command.session,
            cwd: command.cwd,
            host: command.host,
            turn: None,
            item: None,
            provider: None,
            model: None,
        };
        let message = format!("⇄ chosen · this session · {warning}");
        let result = Receipts(shared)
            .chain(
                catalogue::COMMAND_RUN,
                &cx,
                json!({"command":"model","args":"chosen","surface":surface}),
                Next::new(|_| Box::pin(async { Ok(json!({"kind":"applied","message":message})) })),
            )
            .await
            .unwrap();
        serde_json::from_value(result).unwrap()
    }

    #[tokio::test]
    async fn sheet_choice_receipts_leave_no_notice_or_duplicate_toast() {
        let (host, command) = crate::test_support::host();
        let shared = crate::Shared::default();
        shared
            .lock()
            .unwrap()
            .insert(command.session.clone(), Default::default());
        let cx = HookContext {
            session: command.session,
            cwd: command.cwd,
            host: command.host,
            turn: None,
            item: None,
            provider: None,
            model: None,
        };
        for name in ["model", "think"] {
            let result = Receipts(shared.clone())
                .chain(
                    catalogue::COMMAND_RUN,
                    &cx,
                    json!({"command":name,"args":"chosen","surface":"terminal"}),
                    Next::new(|_| {
                        Box::pin(async { Ok(json!({"kind":"applied","message":"⇄ chosen"})) })
                    }),
                )
                .await
                .unwrap();
            assert_eq!(result, json!({"kind":"applied"}));
        }
        crate::pins::announce(
            &CommandContext {
                surface: Some(render::SurfaceName::Terminal),
                session: cx.session,
                cwd: cx.cwd,
                host: cx.host,
                cancel: Default::default(),
            },
            vec!["chosen high".into()],
            Tone::Good,
        )
        .await
        .unwrap();
        assert_eq!(host.calls().iter().filter(|call| matches!(call, testing::HostCall::Signal { kind, .. } if kind == "pin" || kind == "choice")).count(), 1);
    }
}
