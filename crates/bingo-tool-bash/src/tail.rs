//! The progress tail: what a running command shows before it has an answer.
//!
//! A build or a test run says nothing for minutes, so the call's progress line
//! carries the last few lines of its output while it works. The sample is taken
//! on a clock the caller owns; this module only decides what a sample is and
//! whether it is worth sending — an output that has not moved must not wake
//! every surface again.

use std::time::Duration;

use bingo_sdk::ToolContext;
use tokio::sync::Mutex;

use crate::output::Bounded;

/// Lines of a running command's output the tail carries. Enough to see what a
/// build is doing, few enough that it never competes with the transcript.
const LINES: usize = 5;

/// How often the output is sampled while a command runs.
pub const INTERVAL: Duration = Duration::from_millis(100);

/// The progress tail of one call, and the last thing it sent.
#[derive(Debug, Default)]
pub struct Tail {
    sent: Option<String>,
}

impl Tail {
    /// Replace the call's progress tail, if the output has moved since the last
    /// sample. A command that has written nothing yet has no tail to show.
    pub async fn sample(&mut self, output: &Mutex<Bounded>, cx: &ToolContext) {
        let lines = output.lock().await.tail_lines(LINES);
        if lines.is_empty() || self.sent.as_deref() == Some(lines.as_str()) {
            return;
        }
        cx.progress(lines.clone());
        self.sent = Some(lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::context;

    async fn written(text: &str) -> Mutex<Bounded> {
        let output = Mutex::new(Bounded::new(1_000));
        output.lock().await.push(text);
        output
    }

    #[tokio::test]
    async fn the_tail_is_the_output_so_far() {
        let (host, cx) = context();
        let output = written("one\ntwo\n").await;
        Tail::default().sample(&output, &cx).await;
        assert_eq!(host.tails(), vec!["one\ntwo".to_string()]);
    }

    #[tokio::test]
    async fn only_the_last_lines_go_out() {
        let (host, cx) = context();
        let output = written("1\n2\n3\n4\n5\n6\n7\n8\n").await;
        Tail::default().sample(&output, &cx).await;
        assert_eq!(host.tails(), vec!["4\n5\n6\n7\n8".to_string()]);
    }

    #[tokio::test]
    async fn an_output_that_has_not_moved_is_not_sent_again() {
        let (host, cx) = context();
        let output = written("working\n").await;
        let mut tail = Tail::default();
        tail.sample(&output, &cx).await;
        tail.sample(&output, &cx).await;
        output.lock().await.push("done\n");
        tail.sample(&output, &cx).await;
        assert_eq!(
            host.tails(),
            vec!["working".to_string(), "working\ndone".to_string()]
        );
    }

    #[tokio::test]
    async fn a_command_that_has_written_nothing_has_no_tail() {
        let (host, cx) = context();
        let output = written("").await;
        Tail::default().sample(&output, &cx).await;
        assert!(host.tails().is_empty());
    }
}
