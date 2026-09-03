//! The ask door (ADR-0039 §1): a question no tool defines, put to whoever is
//! at the session.
//!
//! Nothing new is opened here. The question rides the same `Msg::Ask` a
//! turn's gate rides and the same attached surface answers it; what this
//! decides is whether anybody is asked at all. The session's policy stands
//! somewhere toward a question it has no call to weigh, and where it stands
//! for itself the question is answered at once — with no interaction and
//! nothing journaled, as a call the gate allows leaves no receipt either.

use bingo_sdk::*;

use super::Host;

/// Put one question, or answer it as this session's stance does.
pub(super) async fn put(
    host: &Host,
    session: &SessionId,
    kind: InteractionKind,
    answers: Vec<AnswerSpec>,
) -> Result<Answer, KernelError> {
    // Live, never reopened: a session that is only stored has nobody at it,
    // and one this host does not run is not answered for at all.
    let mailbox = host.live(session)?.mailbox;
    let refusing = kind.answer_for(AnswerRole::Refusing);
    match host.policy().stance(session).await {
        Stance::Allow => {
            return kind
                .answer_for(AnswerRole::Allowing)
                .ok_or_else(|| unnamed("allowing"));
        }
        Stance::Refuse => return refusing.ok_or_else(|| unnamed("refusing")),
        Stance::Ask => {}
    }
    // Interrupted under, closed under: whatever became of the question, the
    // asker hears the one answer that is safe to hear.
    match mailbox.ask(None, kind, answers).await {
        Ok(answer) => Ok(answer),
        Err(unasked) => refusing.ok_or(unasked),
    }
}

/// A session that answers a question for the person can only answer it in the
/// asker's own words; a question that names none is refused, never guessed at.
fn unnamed(role: &str) -> KernelError {
    KernelError::new(
        ErrorCode::InvalidInput,
        format!("this question names no {role} option"),
    )
}
