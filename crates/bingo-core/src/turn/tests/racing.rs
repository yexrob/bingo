//! One `esc` ends the turn wherever the round happens to be waiting. Each
//! test here stops a turn during one await that used to be bare, and every one
//! of them ends the same way: `Interrupted`, with the marker as the last item
//! and nothing else on the stream.

use super::*;

/// A tool source, a contributor and a turn hook that never answer: whichever
/// await a test stops the turn during, it is this one.
struct NeverAnswers;

#[async_trait]
impl ToolSource for NeverAnswers {
    fn id(&self) -> &str {
        "never"
    }
    async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        std::future::pending().await
    }
}

#[async_trait]
impl ContextContributor for NeverAnswers {
    fn id(&self) -> &str {
        "never"
    }
    fn placement(&self) -> Placement {
        Placement::RoundStart
    }
    async fn contribute(&self, _: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        std::future::pending().await
    }
}

#[async_trait]
impl Hook for NeverAnswers {
    fn id(&self) -> &str {
        "never"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Turn],
            tool: None,
        }
    }
    async fn on_turn(&self, _: Phase, _: &TurnId, _: &[Item], _: &HookContext) {
        std::future::pending().await
    }
}

/// Press esc once the turn is waiting on whatever it is waiting on. The clock
/// is paused, so the wait costs the test nothing.
fn esc_shortly(cancel: &CancellationToken) {
    let cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
}

/// What every one of these turns ends as, whichever await it was stopped
/// during: the one status, and the marker last so a person sees where it ended.
fn stopped(out: &TurnOutcome, host: &RecordingHost) {
    assert_eq!(
        out.status,
        TurnStatus::Interrupted {
            reason: InterruptReason::UserCancel
        }
    );
    assert!(
        matches!(
            out.items.last().map(|item| &item.body),
            Some(ItemBody::Interruption { .. })
        ),
        "the marker is the last item: {:?}",
        out.items
    );
    assert_eq!(host.kinds(), ["completed:interruption/completed"]);
}

/// The provider's `stream` call is the request itself — on a long context,
/// seconds of it — and the token it carries only reaches the stream that does
/// not exist yet.
#[tokio::test(start_paused = true)]
async fn esc_ends_a_turn_whose_request_is_still_being_established() {
    let provider = ScriptedProvider::new(vec![Script::Establishing]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let cancel = CancellationToken::new();
    esc_shortly(&cancel);
    let out = run(&cfg, &host, cancel).await;
    stopped(&out, &host);
    assert_eq!(
        provider.requests().len(),
        1,
        "the request was made; nothing of it ever came back"
    );
}

#[tokio::test(start_paused = true)]
async fn esc_ends_a_turn_a_contributor_is_still_thinking_for() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("never sent"))]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.contributors = ContributorSet::fixed(vec![Arc::new(NeverAnswers)]);
    let host = RecordingHost::new();
    let cancel = CancellationToken::new();
    esc_shortly(&cancel);
    let out = run(&cfg, &host, cancel).await;
    stopped(&out, &host);
    assert!(
        provider.requests().is_empty(),
        "nothing is sent on a context that was never assembled"
    );
}

/// The turn-start case: the tools are resolved before the turn has a round to
/// be in (ADR-0009 §1), and a source that is still answering held every one of
/// the measured seconds.
#[tokio::test(start_paused = true)]
async fn esc_ends_a_turn_still_gathering_its_tools() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("never sent"))]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.tools = ToolSet {
        fixed: Vec::new(),
        sources: vec![Arc::new(NeverAnswers)],
        only: None,
    };
    let host = RecordingHost::new();
    let cancel = CancellationToken::new();
    esc_shortly(&cancel);
    let out = run(&cfg, &host, cancel).await;
    stopped(&out, &host);
    assert!(provider.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn esc_ends_a_turn_waiting_on_an_exact_count() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("never sent"))]).never_counts();
    let mut cfg = config(provider.clone(), vec![]);
    cfg.model
        .as_mut()
        .expect("the scripted model")
        .capabilities
        .count_tokens = true;
    let host = RecordingHost::new();
    let cancel = CancellationToken::new();
    esc_shortly(&cancel);
    let out = run(&cfg, &host, cancel).await;
    stopped(&out, &host);
    assert!(
        provider.requests().is_empty(),
        "the count comes before the request it would have measured"
    );
}

#[tokio::test(start_paused = true)]
async fn esc_ends_a_turn_a_start_hook_is_still_holding() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("never sent"))]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.hooks = HookSet::fixed(vec![Arc::new(NeverAnswers)]);
    let host = RecordingHost::new();
    let cancel = CancellationToken::new();
    esc_shortly(&cancel);
    let out = run(&cfg, &host, cancel).await;
    stopped(&out, &host);
    assert!(provider.requests().is_empty());
}
