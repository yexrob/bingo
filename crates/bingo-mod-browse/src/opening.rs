use bingo_sdk::*;
use serde_json::{Value, json};

pub struct Opening;
#[async_trait::async_trait]
impl Interceptor for Opening {
    fn id(&self) -> &str {
        "browse.rewind"
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher {
            prefilter: json!({"command":"rewind"}),
            ..LinkMatcher::at(catalogue::COMMAND_RUN)
        }
    }
    async fn chain(
        &self,
        _: &catalogue::Entry,
        _: &HookContext,
        value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let input: catalogue::CommandRun =
            serde_json::from_value(value.clone()).map_err(crate::error)?;
        if input.name != "rewind"
            || !input.args.trim().is_empty()
            || input.surface != Some(render::SurfaceName::Terminal)
        {
            return next.run(value).await;
        }
        let reply: catalogue::CommandReply =
            serde_json::from_value(next.run(value).await?).map_err(crate::error)?;
        let fallback = match CommandOutcome::from(reply) {
            CommandOutcome::View { view } => Some(view.fold_markdown()),
            CommandOutcome::Applied { message } => message,
            _ => None,
        };
        serde_json::to_value(CommandOutcome::Surface {
            action: Action {
                name: "Browse".into(),
                args: json!({"confirm":true}),
            },
            fallback,
        })
        .map_err(crate::error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, task};

    #[test]
    fn bare_rewind_requests_the_sheet_and_keeps_the_original_text() {
        for (args, surface, opens) in [
            ("", Some("terminal"), true),
            ("  ", Some("terminal"), true),
            ("turn both", Some("terminal"), false),
            ("", Some("text"), false),
            ("", None, false),
        ] {
            let cx = HookContext {
                session: SessionId::from_raw("s"),
                cwd: "/unused".into(),
                host: testing::ServiceHost::handle(),
                item: None,
                turn: None,
                provider: None,
                model: None,
            };
            let input = json!({"command":"rewind","args":args,"surface":surface});
            let plain = json!(CommandOutcome::View {
                view: View::text("saved turns")
            });
            let expected_input = input.clone();
            let expected_reply = plain.clone();
            let next = Next::new(move |value| {
                assert_eq!(value, expected_input);
                Box::pin(async move { Ok(expected_reply) })
            });
            let mut future = Box::pin(Opening.chain(catalogue::COMMAND_RUN, &cx, input, next));
            let task::Poll::Ready(result) = future
                .as_mut()
                .poll(&mut task::Context::from_waker(task::Waker::noop()))
            else {
                panic!("the next link answers at once");
            };
            let result = result.unwrap();
            if opens {
                assert_eq!(
                    result["action"],
                    json!({"name":"Browse","args":{"confirm":true}})
                );
                assert_eq!(result["fallback"], "saved turns");
            } else {
                assert_eq!(result, plain);
            }
        }
    }
}
