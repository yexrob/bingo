//! Frames in, two streams out. The contract this file keeps is that **stdout
//! carries prose and nothing else**: every diagnostic, tool line and error goes
//! to stderr, and `--output-format json` puts one frame per line on stdout with
//! nothing interleaved. The renderer owns no session state; it reads the folded
//! `SessionState` and remembers only how much of each assistant item it has
//! already written, so a missed delta is caught up at completion.

use std::collections::HashMap;
use std::io::{self, Write};

use bingo_sdk::{
    DeltaKind, ErrorCode, Event, Frame, Item, ItemBody, ItemId, ItemStatus, SessionState,
    TurnStatus,
};
use serde_json::Value;

/// The longest error message a person is asked to read on one line.
const MAX_ERROR_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Prose on stdout, everything else on stderr.
    #[default]
    Text,
    /// One `Frame` per line on stdout, nothing else.
    Json,
}

impl Mode {
    /// `args.outputFormat`, defaulting to text for anything unknown.
    pub fn from_args(args: &Value) -> Self {
        match args.get("outputFormat").and_then(Value::as_str) {
            Some("json") => Mode::Json,
            _ => Mode::Text,
        }
    }
}

/// `[error] code=… msg=…`, the one shape a machine reads errors in.
pub fn error_line(code: ErrorCode, message: &str) -> String {
    let flat = message.replace(['\n', '\r'], " ");
    let msg: String = flat.chars().take(MAX_ERROR_CHARS).collect();
    format!("[error] code={} msg={msg}", code.as_str())
}

#[derive(Debug)]
pub struct Renderer {
    mode: Mode,
    /// Bytes of each assistant item already on stdout.
    written: HashMap<ItemId, usize>,
    /// Stdout ends mid-line, so the next diagnostic owes it a newline.
    open_line: bool,
}

impl Renderer {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            written: HashMap::new(),
            open_line: false,
        }
    }

    pub fn render(
        &mut self,
        frame: &Frame,
        state: &SessionState,
        out: &mut (impl Write + ?Sized),
        err: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        match self.mode {
            Mode::Json => {
                let line = serde_json::to_string(frame).map_err(io::Error::other)?;
                writeln!(out, "{line}")?;
                out.flush()?;
                self.failure(&frame.event, err)
            }
            Mode::Text => self.text(&frame.event, state, out, err),
        }
    }

    fn text(
        &mut self,
        event: &Event,
        state: &SessionState,
        out: &mut (impl Write + ?Sized),
        err: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        match event {
            Event::ItemDelta {
                item,
                kind: DeltaKind::Text,
                data,
                ..
            } => {
                // A text delta is assistant prose by construction; the folded
                // state is the authority whenever it knows the item.
                if state
                    .item(item)
                    .is_none_or(|i| matches!(i.body, ItemBody::Assistant { .. }))
                {
                    *self.written.entry(item.clone()).or_default() += data.len();
                    self.prose(data, out)?;
                }
            }
            Event::ItemStarted { item } => {
                if let ItemBody::ToolCall { name, input, .. } = &item.body {
                    self.diagnostic(&format!("[tool] {name} {}", compact(input)), err)?;
                }
            }
            Event::ItemCompleted { item } => self.completed(item, out, err)?,
            Event::Notice { code, text, .. } => {
                self.diagnostic(&format!("[notice] {code} {text}"), err)?
            }
            Event::TurnRetrying {
                attempt,
                max,
                delay_ms,
                reason,
                ..
            } => self.diagnostic(
                &format!("[retry] attempt {attempt}/{max} in {delay_ms}ms: {reason}"),
                err,
            )?,
            Event::TurnCompleted { .. } | Event::SessionClosed { .. } => {
                self.end_line(out)?;
                self.failure(event, err)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn completed(
        &mut self,
        item: &Item,
        out: &mut (impl Write + ?Sized),
        err: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        match &item.body {
            // The completion is authoritative: deltas can be missed, this cannot.
            ItemBody::Assistant { text } => {
                let written = self.written.get(&item.id).copied().unwrap_or(0);
                if let Some(rest) = text.get(written..)
                    && !rest.is_empty()
                {
                    self.written.insert(item.id.clone(), text.len());
                    self.prose(rest, out)?;
                }
            }
            ItemBody::ToolCall {
                name,
                output,
                duration_ms,
                ..
            } => {
                let failed = item.status == ItemStatus::Failed
                    || output.as_ref().is_some_and(|o| o.is_error);
                let verdict = if failed { "error" } else { "ok" };
                let ms = duration_ms.unwrap_or(0);
                self.diagnostic(&format!("[tool] {name} {verdict} ({ms}ms)"), err)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The one error line, in both modes, for a turn that failed.
    fn failure(&mut self, event: &Event, err: &mut (impl Write + ?Sized)) -> io::Result<()> {
        if let Event::TurnCompleted {
            status: TurnStatus::Failed { error },
            ..
        } = event
        {
            writeln!(err, "{}", error_line(error.code, &error.message))?;
            err.flush()?;
        }
        Ok(())
    }

    fn prose(&mut self, text: &str, out: &mut (impl Write + ?Sized)) -> io::Result<()> {
        out.write_all(text.as_bytes())?;
        out.flush()?;
        self.open_line = !text.ends_with('\n');
        Ok(())
    }

    /// Close the prose line, so the transcript ends with one newline.
    fn end_line(&mut self, out: &mut (impl Write + ?Sized)) -> io::Result<()> {
        if self.open_line {
            out.write_all(b"\n")?;
            out.flush()?;
            self.open_line = false;
        }
        Ok(())
    }

    fn diagnostic(&mut self, line: &str, err: &mut (impl Write + ?Sized)) -> io::Result<()> {
        writeln!(err, "{line}")?;
        err.flush()
    }
}

fn compact(input: &Value) -> String {
    serde_json::to_string(input).unwrap_or_else(|_| String::from("null"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{assistant, frame, session_state, tool_call};
    use bingo_sdk::{
        ContentPart, KernelError, Level, Origin, ToolOutput, TurnId, TurnOrigin, Usage,
    };

    struct Sinks {
        out: Vec<u8>,
        err: Vec<u8>,
    }

    impl Sinks {
        fn out(&self) -> String {
            String::from_utf8_lossy(&self.out).into_owned()
        }

        fn err(&self) -> String {
            String::from_utf8_lossy(&self.err).into_owned()
        }
    }

    /// Fold the frames the way the surface does, rendering each one.
    fn play(mode: Mode, frames: &[Frame]) -> Sinks {
        let mut state = session_state();
        let mut renderer = Renderer::new(mode);
        let mut sinks = Sinks {
            out: Vec::new(),
            err: Vec::new(),
        };
        for frame in frames {
            state.apply(frame);
            renderer
                .render(frame, &state, &mut sinks.out, &mut sinks.err)
                .expect("writing to a vector cannot fail");
        }
        sinks
    }

    fn text_turn() -> Vec<Frame> {
        vec![
            frame(
                1,
                Event::TurnStarted {
                    turn: TurnId::from_raw("trn_1"),
                    inputs: vec![],
                    origin: TurnOrigin::Submit,
                },
            ),
            frame(
                2,
                Event::ItemStarted {
                    item: assistant("itm_1", "", ItemStatus::Running),
                },
            ),
            frame(
                3,
                Event::ItemDelta {
                    item: ItemId::from_raw("itm_1"),
                    n: 0,
                    kind: DeltaKind::Text,
                    data: "Hel".into(),
                },
            ),
            frame(
                4,
                Event::ItemDelta {
                    item: ItemId::from_raw("itm_1"),
                    n: 1,
                    kind: DeltaKind::Text,
                    data: "lo".into(),
                },
            ),
            frame(
                5,
                Event::ItemCompleted {
                    item: assistant("itm_1", "Hello", ItemStatus::Completed),
                },
            ),
            frame(
                6,
                Event::TurnCompleted {
                    turn: TurnId::from_raw("trn_1"),
                    status: TurnStatus::Completed,
                    usage: Usage::default(),
                },
            ),
        ]
    }

    #[test]
    fn text_mode_prints_the_deltas_once_and_ends_with_one_newline() {
        let sinks = play(Mode::Text, &text_turn());
        assert_eq!(sinks.out(), "Hello\n");
        assert_eq!(sinks.err(), "");
    }

    #[test]
    fn a_completion_longer_than_the_deltas_prints_only_the_remainder() {
        let mut frames = text_turn();
        frames[4] = frame(
            5,
            Event::ItemCompleted {
                item: assistant("itm_1", "Hello, world", ItemStatus::Completed),
            },
        );
        assert_eq!(play(Mode::Text, &frames).out(), "Hello, world\n");
    }

    #[test]
    fn an_item_completed_with_no_deltas_at_all_still_prints() {
        let frames = vec![
            frame(
                1,
                Event::ItemCompleted {
                    item: assistant("itm_1", "silent", ItemStatus::Completed),
                },
            ),
            frame(
                2,
                Event::TurnCompleted {
                    turn: TurnId::from_raw("trn_1"),
                    status: TurnStatus::Completed,
                    usage: Usage::default(),
                },
            ),
        ];
        assert_eq!(play(Mode::Text, &frames).out(), "silent\n");
    }

    #[test]
    fn tool_calls_notices_and_retries_are_stderr_only() {
        let frames = vec![
            frame(
                1,
                Event::ItemStarted {
                    item: tool_call("itm_2", "Read", None, ItemStatus::Running),
                },
            ),
            frame(
                2,
                Event::Notice {
                    level: Level::Warn,
                    code: "COUNT_TOKENS_UNAVAILABLE".into(),
                    text: "estimating".into(),
                },
            ),
            frame(
                3,
                Event::TurnRetrying {
                    turn: TurnId::from_raw("trn_1"),
                    attempt: 1,
                    max: 10,
                    delay_ms: 500,
                    dropped: vec![],
                    reason: "server error 503".into(),
                },
            ),
            frame(
                4,
                Event::ItemCompleted {
                    item: tool_call(
                        "itm_2",
                        "Read",
                        Some(ToolOutput::text("ok")),
                        ItemStatus::Completed,
                    ),
                },
            ),
        ];
        let sinks = play(Mode::Text, &frames);
        assert_eq!(sinks.out(), "", "stdout carries prose and nothing else");
        assert_eq!(
            sinks.err(),
            concat!(
                "[tool] Read {\"file_path\":\"Cargo.toml\"}\n",
                "[notice] COUNT_TOKENS_UNAVAILABLE estimating\n",
                "[retry] attempt 1/10 in 500ms: server error 503\n",
                "[tool] Read ok (12ms)\n",
            )
        );
    }

    #[test]
    fn a_failed_tool_reports_error_with_its_duration() {
        let frames = vec![frame(
            1,
            Event::ItemCompleted {
                item: tool_call(
                    "itm_2",
                    "Read",
                    Some(ToolOutput::error("file not found: a.txt")),
                    ItemStatus::Failed,
                ),
            },
        )];
        assert_eq!(
            play(Mode::Text, &frames).err(),
            "[tool] Read error (12ms)\n"
        );
    }

    #[test]
    fn a_failed_turn_writes_one_error_line_to_stderr() {
        let frames = vec![frame(
            1,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Failed {
                    error: KernelError::new(
                        ErrorCode::ProviderUnavailable,
                        "the provider said\nno\r\nrepeatedly",
                    ),
                },
                usage: Usage::default(),
            },
        )];
        let sinks = play(Mode::Text, &frames);
        assert_eq!(sinks.out(), "");
        assert_eq!(
            sinks.err(),
            "[error] code=PROVIDER_UNAVAILABLE msg=the provider said no  repeatedly\n"
        );
    }

    #[test]
    fn a_long_error_message_is_cut_at_two_hundred_characters() {
        let line = error_line(ErrorCode::Internal, &"x".repeat(500));
        let msg = line
            .strip_prefix("[error] code=INTERNAL msg=")
            .expect("prefix");
        assert_eq!(msg.chars().count(), MAX_ERROR_CHARS);
    }

    #[test]
    fn json_mode_puts_one_frame_per_line_on_stdout_and_nothing_else() {
        let frames = text_turn();
        let sinks = play(Mode::Json, &frames);
        let rendered = sinks.out();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), frames.len(), "ephemeral frames included");
        for (line, source) in lines.iter().zip(&frames) {
            let parsed: Frame = serde_json::from_str(line).expect("each line is a Frame");
            assert_eq!(&parsed, source);
        }
        assert_eq!(sinks.err(), "");
    }

    #[test]
    fn json_mode_still_reports_a_failed_turn_on_stderr() {
        let frames = vec![frame(
            1,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Failed {
                    error: KernelError::new(ErrorCode::ToolFailed, "boom"),
                },
                usage: Usage::default(),
            },
        )];
        let sinks = play(Mode::Json, &frames);
        assert_eq!(sinks.err(), "[error] code=TOOL_FAILED msg=boom\n");
        assert_eq!(sinks.out().lines().count(), 1);
    }

    #[test]
    fn the_output_format_argument_picks_the_mode() {
        assert_eq!(
            Mode::from_args(&serde_json::json!({"outputFormat": "json"})),
            Mode::Json
        );
        assert_eq!(
            Mode::from_args(&serde_json::json!({"outputFormat": "text"})),
            Mode::Text
        );
        assert_eq!(Mode::from_args(&Value::Null), Mode::Text);
    }

    #[test]
    fn a_stale_delta_for_a_tool_item_never_reaches_stdout() {
        let frames = vec![
            frame(
                1,
                Event::ItemStarted {
                    item: tool_call("itm_2", "Read", None, ItemStatus::Running),
                },
            ),
            frame(
                2,
                Event::ItemDelta {
                    item: ItemId::from_raw("itm_2"),
                    n: 0,
                    kind: DeltaKind::Tail,
                    data: "reading…".into(),
                },
            ),
        ];
        assert_eq!(play(Mode::Text, &frames).out(), "");
    }

    #[test]
    fn a_user_item_is_not_echoed() {
        let mut item = assistant("itm_0", "", ItemStatus::Completed);
        item.body = ItemBody::User {
            parts: vec![ContentPart::text("hi")],
            origin: Origin::surface("print"),
        };
        let frames = vec![frame(1, Event::ItemCompleted { item })];
        assert_eq!(play(Mode::Text, &frames).out(), "");
    }
}
