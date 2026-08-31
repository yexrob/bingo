//! Where a running command's output goes.
//!
//! A foreground call collects into memory, bounded, and answers with the whole
//! of it; a background job appends to its log, which is the one representation
//! of what it wrote (ADR-0018 §3). Promotion is the single move between the
//! two: what the buffer holds is written into the log and the same pipes carry
//! on into the file. The readers hold the sink behind a lock and never learn
//! that it changed, which is why nothing restarts.

use crate::log::Log;
use crate::output::Bounded;

#[derive(Debug)]
pub enum Sink {
    /// A call is waiting for this command, and will read what it wrote.
    Buffer(Bounded),
    /// Nobody is waiting: the file is the output.
    File(Log),
}

impl Sink {
    pub fn buffer(max: usize) -> Self {
        Sink::Buffer(Bounded::new(max))
    }

    pub fn file(log: Log) -> Self {
        Sink::File(log)
    }

    pub async fn push(&mut self, text: &str) {
        match self {
            Sink::Buffer(bounded) => bounded.push(text),
            // A log that cannot be written is not a reason to stop the
            // command; the read that follows will find the file short.
            Sink::File(log) => {
                if let Err(error) = log.write(text).await {
                    tracing::warn!(%error, "a job's log could not be written");
                }
            }
        }
    }

    /// The last `n` lines, for a call's progress line. A file has no reader
    /// waiting on it, so it shows nothing.
    pub fn tail_lines(&self, n: usize) -> String {
        match self {
            Sink::Buffer(bounded) => bounded.tail_lines(n),
            Sink::File(_) => String::new(),
        }
    }

    /// Everything a waiting call gets back.
    pub fn finish(&self) -> String {
        match self {
            Sink::Buffer(bounded) => bounded.finish(),
            Sink::File(_) => String::new(),
        }
    }

    /// Hand the command to its log: what the buffer holds goes in first, so
    /// the file is the whole story from the moment the command started.
    pub async fn promote(&mut self, mut log: Log) {
        if let Sink::Buffer(bounded) = self
            && let Err(error) = log.write(&bounded.finish()).await
        {
            tracing::warn!(%error, "a promoted command's output could not be written");
        }
        *self = Sink::file(log);
    }

    /// The log this sink writes to, when it has one.
    pub fn log(&mut self) -> Option<&mut Log> {
        match self {
            Sink::Buffer(_) => None,
            Sink::File(log) => Some(log),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log;

    async fn log_in(dir: &tempfile::TempDir) -> Log {
        Log::create(dir.path(), "job").await.expect("a log")
    }

    async fn text_of(sink: &mut Sink) -> String {
        let path = sink.log().expect("a file sink").path().to_path_buf();
        log::window(&path, 0, 100_000).await.expect("a window").text
    }

    #[tokio::test]
    async fn a_buffer_answers_the_call_that_is_waiting() {
        let mut sink = Sink::buffer(1_000);
        sink.push("one\ntwo\n").await;
        assert_eq!(sink.finish(), "one\ntwo\n");
        assert_eq!(sink.tail_lines(1), "two");
    }

    #[tokio::test]
    async fn a_file_sink_keeps_nothing_in_memory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut sink = Sink::file(log_in(&dir).await);
        sink.push("running\n").await;
        assert_eq!(sink.finish(), "", "the file is the output");
        assert_eq!(sink.tail_lines(5), "", "nobody is watching a job");
        assert_eq!(text_of(&mut sink).await, "running\n");
    }

    /// The whole point of promotion: the command does not restart, so what it
    /// had already written must be at the head of its log.
    #[tokio::test]
    async fn promotion_carries_what_the_buffer_held_into_the_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut sink = Sink::buffer(1_000);
        sink.push("before\n").await;
        sink.promote(log_in(&dir).await).await;
        sink.push("after\n").await;
        assert_eq!(text_of(&mut sink).await, "before\nafter\n");
    }

    #[tokio::test]
    async fn promoting_a_sink_that_is_already_a_file_keeps_writing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut sink = Sink::file(log_in(&dir).await);
        sink.push("one\n").await;
        sink.promote(Log::create(dir.path(), "second").await.expect("a log"))
            .await;
        sink.push("two\n").await;
        assert_eq!(text_of(&mut sink).await, "two\n");
    }
}
