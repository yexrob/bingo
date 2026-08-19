//! The answer to a question put to the session actor, and the two ways to take
//! it.
//!
//! A registry call that returns a value cannot be a one-way report: the caller
//! needs what came back. Its callers, though, are not one kind of thing. An
//! engine task awaits; the actor's own loop and a `spawn_blocking` worker
//! cannot. [`Answer`] serves both without giving the same question two
//! spellings: `.await` for the first, [`Answer::now`] for the second.
//!
//! Blocking is safe here for one structural reason, and only that one: the actor
//! runs on a thread of its own and never waits on a frontend or an engine task.
//! A thread that blocks on it is waiting on a bounded amount of work, exactly as
//! it used to block on the registry mutex this replaced. (Bounded is not
//! instant: a few action handlers write settings from inside the loop, so the
//! wait can include a small synchronous file write. That is a latency the whole
//! session shares, not a wait on anything that could wait back.)
//!
//! The one shape that would deadlock is a call *on the actor's thread* that has
//! to wait: the actor cannot answer itself. The calls the loop makes are all to
//! registries it owns, inline, which fill the reply before returning — so they
//! never park. [`block_on`] asserts that rather than trusting it, because the
//! failure mode is a hang with no timeout and no message.
//!
//! **The render thread is no longer one of them** (D154). The terminal front end
//! used to take every answer this way, on the thread that draws; it records an
//! intent instead and the loop performs it (`tui::intent`). What is left of
//! `now` is the actor reading its own tables inside its own message, the blocking
//! workers an engine spawns, and — the one exception, and it is a real one —
//! `/team start`, `/team assign` and `/team stop`, which the console still
//! dispatches synchronously into `team_cmd`. Those park a runtime worker for the
//! length of a crew coming up. Nothing deadlocks (the actor is its own thread
//! and stays answering), but it is a stall, and the honest place to say so is
//! here rather than in a D record nobody greps.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use tokio::sync::oneshot;

/// One pending answer.
///
/// Marked `must_use` on purpose: the question has already been sent by the time
/// this exists, so dropping it silently turns a question into a report — which
/// is sometimes what a caller means and never what it should mean by accident.
#[must_use = "an answer that is neither awaited nor taken turns the question into a report"]
pub struct Answer<T> {
    reply: oneshot::Receiver<T>,
    /// What to answer when the actor is gone. The actor outlives every handle,
    /// so this is the shutdown path and nothing else — but a caller still gets a
    /// value rather than a panic, because a session ending is not an error in
    /// the thing that asked.
    gone: Option<T>,
}

impl<T> Answer<T> {
    pub(crate) fn new(reply: oneshot::Receiver<T>, gone: T) -> Self {
        Self {
            reply,
            gone: Some(gone),
        }
    }

    /// The answer, blocking this thread until the actor has it.
    pub fn now(self) -> T
    where
        T: Unpin,
    {
        block_on(self)
    }

    /// The question was a statement: the message is already on its way and the
    /// caller does not want the receipt.
    ///
    /// A oneshot needs no cancellation — dropping the receiver is exactly this —
    /// so the only thing here is saying so, because `Answer` is `#[must_use]`
    /// precisely to stop that happening by accident.
    pub fn forget(self) {}
}

impl<T: Unpin> Future for Answer<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        match Pin::new(&mut this.reply).poll(cx) {
            Poll::Ready(Ok(answer)) => Poll::Ready(answer),
            Poll::Ready(Err(_)) => Poll::Ready(
                this.gone
                    .take()
                    .unwrap_or_else(|| unreachable!("an answer is taken once")),
            ),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Block this thread until a future is ready, using std's parking rather than
/// the runtime's.
///
/// The runtime's own `block_on` refuses to run inside itself, and this is called
/// from inside it — a key handler on a worker thread. It is sound because of
/// what it waits on, not because of where it is called: see the module note.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => {
                // The actor cannot answer itself. Everything the loop asks of
                // its own registries is filled inline, so it never reaches here
                // — and if a refactor ever routes one of those through a handle
                // instead, this says which invariant broke rather than hanging
                // the session forever with nothing on the screen.
                assert!(
                    std::thread::current().name() != Some(ACTOR_THREAD),
                    "the session actor blocked on an answer only it can give"
                );
                std::thread::park()
            }
        }
    }
}

/// The name the session actor's thread runs under, which is how the guard above
/// recognises it. Set in `controller::spawn`.
pub(crate) const ACTOR_THREAD: &str = "bingo-session";

#[cfg(test)]
mod tests {
    use super::*;

    /// The same answer, taken both ways.
    #[tokio::test]
    async fn an_answer_can_be_awaited_or_waited_for() {
        let (reply, answer) = oneshot::channel();
        let _ = reply.send(7);
        assert_eq!(Answer::new(answer, 0).await, 7);

        let (reply, answer) = oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let _ = reply.send(7);
        });
        assert_eq!(Answer::new(answer, 0).now(), 7);
    }

    /// The guard fires on the thread it is about, and on no other. A worker
    /// parking here is the ordinary case and must stay ordinary.
    #[test]
    fn the_actor_may_not_wait_for_an_answer_only_it_can_give() {
        let held = std::thread::Builder::new()
            .name(ACTOR_THREAD.to_string())
            .spawn(|| {
                let (reply, answer) = oneshot::channel::<u8>();
                // Held, so nothing can answer and nothing can drop the sender:
                // without the guard this thread parks for ever.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Answer::new(answer, 0).now()
                }));
                drop(reply);
                outcome.is_err()
            })
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            held.join().unwrap_or(false),
            "the actor's own thread is refused rather than hung"
        );

        let (reply, answer) = oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let _ = reply.send(9);
        });
        assert_eq!(
            Answer::new(answer, 0).now(),
            9,
            "every other thread waits as it always did"
        );
    }

    /// A session that ended answers rather than panicking in whatever was
    /// mid-question when it did.
    #[tokio::test]
    async fn a_gone_actor_answers_with_the_shutdown_value() {
        let (reply, answer) = oneshot::channel::<u8>();
        drop(reply);
        assert_eq!(Answer::new(answer, 3).await, 3);
    }
}
