use bingo_sdk::*;
use serde_json::{Value, json};
pub struct Open(pub crate::Shared);
#[async_trait::async_trait]
impl Command for Open {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "crew".into(),
            aliases: vec![],
            hint: "agents, rooms, tasks and team in one panel".into(),
            args: ArgSpec::Words {
                values: crate::state::Tab::ALL
                    .into_iter()
                    .map(|tab| tab.label().into())
                    .collect(),
                labels: Default::default(),
                then: None,
                alone: vec![],
                person: vec![],
            },
            instant: true,
            family: "terminal".into(),
            hidden: false,
            opens: true,
            keys: vec!["ctrl+n".into()],
        }
    }
    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let tab = if args.trim().is_empty() {
            crate::state::Tab::default()
        } else {
            crate::state::Tab::parse(args.trim())
                .ok_or_else(|| crate::error("Use /crew [agents|rooms|tasks|team]."))?
        };
        open(
            &self.0,
            &cx.host,
            &cx.session,
            tab,
            Some("Use /agents, /room, /tasks and /team for the plain tables.".into()),
        )
        .await
    }
}

pub async fn open(
    shared: &crate::Shared,
    host: &HostHandle,
    session: &SessionId,
    tab: crate::state::Tab,
    fallback: Option<String>,
) -> Result<CommandOutcome, KernelError> {
    shared.lock().map_err(crate::error)?.insert(
        session.clone(),
        crate::state::Panel {
            tab,
            ..Default::default()
        },
    );
    host.signal(session, crate::ID, crate::REQUEST, slot())
        .await?;
    host.invalidate(session, vec![crate::REQUEST.into()])
        .await?;
    Ok(CommandOutcome::Surface {
        action: Action {
            name: "Pane".into(),
            args: json!({"plugin":crate::ID,"id":crate::REQUEST}),
        },
        fallback,
    })
}
pub fn slot() -> Value {
    let mut size = slot::PaneSize::from_rows(18);
    size.rows.max = slot::Length::Percent(80);
    size.columns.ideal = slot::Length::Percent(100);
    slot::with(
        json!({}),
        [slot::Slot::Pane {
            hints: None,
            id: crate::REQUEST.into(),
            title: "crew".into(),
            focus: false,
            close_on_escape: false,
            hold_toasts: false,
            asked: true,
            place: slot::above(),
            size,
            view: slot::Shown::Pull {
                request: render::PullRequest {
                    component: render::Component::Pane,
                    request_id: crate::REQUEST.into(),
                    props: json!({}),
                    viewport: render::Viewport {
                        place: None,
                        columns: 0,
                        rows: 0,
                    },
                },
            },
        }],
    )
}
pub async fn close(cx: &CommandContext, shared: &crate::Shared) -> Result<(), KernelError> {
    shared.lock().map_err(crate::error)?.remove(&cx.session);
    cx.host
        .signal(&cx.session, crate::ID, crate::REQUEST, Value::Null)
        .await
}
