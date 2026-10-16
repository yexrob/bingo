use super::*;

struct Observer {
    id: &'static str,
    point: HookPoint,
    seen: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl Hook for Observer {
    fn id(&self) -> &str {
        self.id
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![self.point],
            tool: (self.point == HookPoint::AfterTool).then_some("Echo*".into()),
        }
    }
    async fn after_tool(&self, call: &ToolCall, _: &ToolOutput, _: &HookContext) -> HookOutcome {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.id, call.name));
        HookOutcome::Block {
            reason: "enough".into(),
        }
    }
    async fn on_stop(&self, _: &HookContext) -> HookOutcome {
        let mut seen = self.seen.lock().unwrap();
        if seen.is_empty() {
            seen.push(self.id.into());
            HookOutcome::Block {
                reason: "again".into(),
            }
        } else {
            HookOutcome::Continue
        }
    }
}
fn observers(point: HookPoint, seen: &Arc<Mutex<Vec<String>>>) -> HookSet {
    HookSet::fixed(
        ["first", "second"]
            .into_iter()
            .map(|id| {
                Arc::new(Observer {
                    id,
                    point,
                    seen: seen.clone(),
                }) as Arc<dyn Hook>
            })
            .collect(),
    )
}
#[tokio::test]
async fn after_every_matched_hook_sees_every_result_and_block_ends_the_round() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut events = tool_call("Echo", json!({"v": 1}));
    events.insert(
        1,
        Ok(ModelEvent::ToolCall {
            id: "c2".into(),
            name: "Echo".into(),
            input: json!({"v": 2}).to_string(),
        }),
    );
    let provider = ScriptedProvider::new(vec![Script::Events(events)]);
    let mut cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    cfg.hooks = observers(HookPoint::AfterTool, &seen);
    let outcome = run(&cfg, &RecordingHost::new(), CancellationToken::new()).await;
    assert_eq!(outcome.status, TurnStatus::Completed);
    assert_eq!(
        *seen.lock().unwrap(),
        ["first:Echo", "second:Echo", "first:Echo", "second:Echo"]
    );
    assert_eq!(provider.requests().len(), 1);
}
#[tokio::test]
async fn stop_first_block_adds_its_origin_and_another_round() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider::new(vec![
        Script::Events(text("one")),
        Script::Events(text("two")),
    ]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.hooks = observers(HookPoint::Stop, &seen);
    let host = RecordingHost::new();
    let result = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(result.status, TurnStatus::Completed);
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(*seen.lock().unwrap(), ["first"]);
    assert!(
        host.events()
            .iter()
            .any(|event| matches!(event, Event::ItemCompleted { item }
        if matches!(&item.body, ItemBody::User { origin, .. } if origin.surface == "hook:first")))
    );
}
