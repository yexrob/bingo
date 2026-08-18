//! The answer to a question put to the session actor, and the two ways to take
//! it.
//!
//! A registry call that returns a value cannot be a one-way report: the caller
//! needs what came back. Its callers, though, are not one kind of thing. An
//! engine task awaits; a key handler and a render pass cannot, because the whole
//! terminal front end above them is synchronous and will stay that way until B7
//! turns it into an attachment. [`Answer`] serves both without giving the same
//! question two spellings: `.await` for the first, [`Answer::now`] for the
//! second.
//!
//! Blocking is safe here for one structural reason, and only that one: the actor
//! runs on a thread of its own and never waits on anything — not a frontend, not
//! an engine task, not the disk. A thread that blocks on it is therefore waiting
//! on a bounded amount of arithmetic, exactly as it used to block on the
//! registry mutex this replaced. **B7 removes the blocking half.**

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use tokio::sync::oneshot;

/// One pending answer.
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
fn block_on<F: Future>(future: F) -> F::Output {
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
            Poll::Pending => std::thread::park(),
        }
    }
}

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

    /// A session that ended answers rather than panicking in whatever was
    /// mid-question when it did.
    #[tokio::test]
    async fn a_gone_actor_answers_with_the_shutdown_value() {
        let (reply, answer) = oneshot::channel::<u8>();
        drop(reply);
        assert_eq!(Answer::new(answer, 3).await, 3);
    }
}
