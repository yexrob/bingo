//! Frames in, two streams out. The contract this file keeps is that **stdout
//! carries prose and nothing else**: every diagnostic, tool line and error goes
//! to stderr, and `--output-format json` puts one frame per line on stdout with
//! nothing interleaved. The renderer owns no session state; it reads the folded
//! `SessionState` and remembers only how much of each assistant item it has
//! already written, so a missed delta is caught up at completion.

use std::collections::HashMap;
use std::io::{self, Write};

use bingo_sdk::{
    DeltaKind, ErrorCode, Event, Frame, IntentOutcome, Item, ItemBody, ItemId, ItemStatus,
    SessionState, ToolOutput, TurnStatus,
};
use serde_json::Value;

use crate::stream_json::Encoder;

/// The longest error message a person is asked to read on one line.
const MAX_ERROR_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Prose on stdout, everything else on stderr.
    #[default]
    Text,
    /// One `Frame` per line on stdout, nothing else.
    Json,
    /// Claude Code's envelope on stdout, one object per line (ADR-0007 §8).
    StreamJson,
}

impl Mode {
    /// `args.outputFormat`, defaulting to text for anything unknown.
    pub fn from_args(args: &Value) -> Self {
        match args.get("outputFormat").and_then(Value::as_str) {
            Some("json") => Mode::Json,
            Some("stream-json") => Mode::StreamJson,
            _ => Mode::Text,
        }
    }
}

/// One failure, one line. A person at the terminal reads prose; a program
/// on the other end of a pipe reads `[error] code=… msg=…`, the contract
/// hosts parse.
pub fn error_report(code: ErrorCode, message: &str, human: bool) -> String {
    let flat = message.replace(['\n', '\r'], " ");
    let msg: String = flat.chars().take(MAX_ERROR_CHARS).collect();
    if human {
        format!("error: {msg}")
    } else {
        format!("[error] code={} msg={msg}", code.as_str())
    }
}

/// A startup notice in the same two registers.
pub fn notice_report(code: &str, text: &str, human: bool) -> String {
    if human {
        format!("note: {text}")
    } else {
        format!("[notice] {code} {text}")
    }
}

#[derive(Debug)]
pub struct Renderer {
    output: Output,
    /// Stderr is a terminal: diagnostics are for a person.
    human: bool,
    /// Bytes of each assistant item already on stdout.
    written: HashMap<ItemId, usize>,
    /// Stdout ends mid-line, so the next diagnostic owes it a newline.
    open_line: bool,
}

/// The mode holding the state it needs, so the renderer never carries a second
/// copy of which mode it is in.
#[derive(Debug)]
enum Output {
    Text,
    Json,
    Stream(Encoder),
}

impl Renderer {
    /// `tools` names the catalogue for the stream-json preamble; no other mode
    /// reads it, and only the host knows it.
    pub fn new(mode: Mode, human: bool, tools: Vec<String>) -> Self {
        Self {
            output: match mode {
                Mode::Text => Output::Text,
                Mode::Json => Output::Json,
                Mode::StreamJson => Output::Stream(Encoder::new(tools)),
            },
            human,
            written: HashMap::new(),
            open_line: false,
        }
    }

    /// The preamble a mode owes before any frame: the stream-json `init` line,
    /// and nothing at all otherwise.
    pub fn open(&self, state: &SessionState, out: &mut (impl Write + ?Sized)) -> io::Result<()> {
        let Output::Stream(encoder) = &self.output else {
            return Ok(());
        };
        write_line(&encoder.init(state).to_string(), out)
    }

    /// `state` is the frame's own session, `root` the one the run opened;
    /// they differ only for a sub-session's frame.
    pub fn render(
        &mut self,
        frame: &Frame,
        state: &SessionState,
        root: &SessionState,
        out: &mut (impl Write + ?Sized),
        err: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        if !self.reports(state, root) {
            return Ok(());
        }
        if matches!(self.output, Output::Text) {
            return self.text(&frame.event, state, out, err);
        }
        self.machine(frame, state, root, out)?;
        self.failure(&frame.event, err)
    }

    /// Text and json report the root alone; only the envelope has a shape
    /// for a sub-session's lines (`parent_tool_use_id`, ADR-0010 §4). The run
    /// still folds and answers the whole tree — that is not the renderer's
    /// business.
    fn reports(&self, state: &SessionState, root: &SessionState) -> bool {
        matches!(self.output, Output::Stream(_)) || state.summary.id == root.summary.id
    }

    /// The two machine-readable modes: at most one line per frame on stdout.
    fn machine(
        &mut self,
        frame: &Frame,
        state: &SessionState,
        root: &SessionState,
        out: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        match &mut self.output {
            Output::Json => {
                let line = serde_json::to_string(frame).map_err(io::Error::other)?;
                write_line(&line, out)
            }
            Output::Stream(encoder) => match encoder.line(frame, state, root) {
                Some(line) => write_line(&line.to_string(), out),
                None => Ok(()),
            },
            // Rendered as prose before this is reached.
            Output::Text => Ok(()),
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
            } => self.delta(item, data, state, out)?,
            Event::ItemStarted { item } => self.started(item, err)?,
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
            // A command's receipt: what an instant `/command` answered with.
            Event::IntentAck {
                outcome: IntentOutcome::Applied { result },
                ..
            } => self.applied(result, out)?,
            // Deliberately silent: state a headless run has no use for.
            Event::ItemDelta { .. }
            | Event::ItemUpdated { .. }
            | Event::SessionUpdated { .. }
            | Event::TurnStarted { .. }
            | Event::TurnUsage { .. }
            | Event::QueueChanged { .. }
            | Event::InteractionOpened { .. }
            | Event::InteractionResolved { .. }
            | Event::InteractionCancelled { .. }
            | Event::IntentAck { .. }
            | Event::Compacted { .. }
            | Event::Rewound { .. }
            | Event::ConfigChanged { .. }
            | Event::CatalogChanged { .. }
            | Event::Extension { .. }
            | Event::Signal { .. }
            | Event::Lagged { .. } => {}
        }
        Ok(())
    }

    /// A text delta is assistant prose by construction; the folded state is the
    /// authority whenever it knows the item.
    fn delta(
        &mut self,
        item: &ItemId,
        data: &str,
        state: &SessionState,
        out: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        if state
            .item(item)
            .is_none_or(|i| matches!(i.body, ItemBody::Assistant { .. }))
        {
            *self.written.entry(item.clone()).or_default() += data.len();
            self.prose(data, out)?;
        }
        Ok(())
    }

    /// A tool call announces its input when it starts; its verdict comes at completion.
    fn started(&mut self, item: &Item, err: &mut (impl Write + ?Sized)) -> io::Result<()> {
        if let ItemBody::ToolCall { name, input, .. } = &item.body {
            self.diagnostic(&format!("[tool] {name} {}", compact(input)), err)?;
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
            } => self.tool_done(item, name, output.as_ref(), *duration_ms, err)?,
            ItemBody::Notice { code, text, .. } => {
                self.diagnostic(&format!("[notice] {code} {text}"), err)?;
            }
            ItemBody::Interruption { marker } => self.diagnostic(marker, err)?,
            // Not part of a headless transcript.
            ItemBody::User { .. }
            | ItemBody::Reasoning { .. }
            | ItemBody::Action { .. }
            | ItemBody::Compaction { .. }
            | ItemBody::Rewind { .. }
            | ItemBody::QuestionAnswer { .. }
            | ItemBody::PermissionReceipt { .. }
            | ItemBody::Asset { .. } => {}
        }
        Ok(())
    }

    /// The verdict line, then what a person would have seen (ADR-0013 §2):
    /// the display's fold, indented under it.
    fn tool_done(
        &mut self,
        item: &Item,
        name: &str,
        output: Option<&ToolOutput>,
        duration_ms: Option<u64>,
        err: &mut (impl Write + ?Sized),
    ) -> io::Result<()> {
        let verdict = if tool_failed(item, output) {
            "error"
        } else {
            "ok"
        };
        let ms = duration_ms.unwrap_or(0);
        self.diagnostic(&format!("[tool] {name} {verdict} ({ms}ms)"), err)?;
        if let Some(view) = output.and_then(|o| o.display.as_ref()) {
            for line in view.fold().lines() {
                self.diagnostic(&format!("  {line}"), err)?;
            }
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
            writeln!(
                err,
                "{}",
                error_report(error.code, &error.message, self.human)
            )?;
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

    /// A command's answer on stdout: its message, or its view's fold. An
    /// item result is already in the transcript, and a turn's ack says
    /// nothing the turn will not say itself.
    fn applied(&mut self, result: &Value, out: &mut (impl Write + ?Sized)) -> io::Result<()> {
        if let Some(message) = result.get("message").and_then(Value::as_str) {
            return self.prose(&format!("{message}\n"), out);
        }
        if let Some(view) = result.get("view")
            && let Ok(view) = serde_json::from_value::<bingo_sdk::View>(view.clone())
        {
            return self.prose(&format!("{}\n", view.fold()), out);
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

/// One line on stdout, flushed: a host reads this stream as it arrives.
pub(crate) fn write_line(line: &str, out: &mut (impl Write + ?Sized)) -> io::Result<()> {
    writeln!(out, "{line}")?;
    out.flush()
}

/// The verdict both modes report for a finished tool call, in one place: a
/// failed status and an error output are the same news.
pub(crate) fn tool_failed(item: &Item, output: Option<&ToolOutput>) -> bool {
    item.status == ItemStatus::Failed || output.is_some_and(|o| o.is_error)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::{assistant, frame, session_state, tool_call};
    use bingo_sdk::{
        ContentPart, KernelError, Level, Origin, SessionId, TurnId, TurnOrigin, Usage,
    };

    pub(crate) struct Sinks {
        out: Vec<u8>,
        err: Vec<u8>,
    }

    impl Sinks {
        pub(crate) fn out(&self) -> String {
            String::from_utf8_lossy(&self.out).into_owned()
        }

        pub(crate) fn err(&self) -> String {
            String::from_utf8_lossy(&self.err).into_owned()
        }
    }

    /// Fold the frames the way the surface does, rendering each one.
    pub(crate) fn play(mode: Mode, frames: &[Frame]) -> Sinks {
        play_with(mode, frames, Vec::new())
    }

    /// The same, for a mode that reads the tool catalogue.
    pub(crate) fn play_with(mode: Mode, frames: &[Frame], tools: Vec<String>) -> Sinks {
        let mut state = session_state();
        let mut renderer = Renderer::new(mode, false, tools);
        let mut sinks = Sinks {
            out: Vec::new(),
            err: Vec::new(),
        };
        renderer
            .open(&state, &mut sinks.out)
            .expect("writing to a vector cannot fail");
        for frame in frames {
            state.apply(frame);
            renderer
                .render(frame, &state, &state, &mut sinks.out, &mut sinks.err)
                .expect("writing to a vector cannot fail");
        }
        sinks
    }

    pub(crate) fn text_turn() -> Vec<Frame> {
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
    fn a_display_view_is_printed_as_its_fold_under_the_verdict() {
        let output = ToolOutput {
            display: Some(bingo_sdk::View::Diff {
                unified: "@@ -1 +1 @@\n-alpha\n+beta\n".into(),
            }),
            ..ToolOutput::text("edited greeting.txt")
        };
        let frames = vec![frame(
            1,
            Event::ItemCompleted {
                item: tool_call("itm_2", "Edit", Some(output), ItemStatus::Completed),
            },
        )];
        assert_eq!(
            play(Mode::Text, &frames).err(),
            "[tool] Edit ok (12ms)\n  @@ -1 +1 @@\n  -alpha\n  +beta\n"
        );
    }

    /// A word this binary has no name for still reaches a person (ADR-0038
    /// §2): the display reads as the fold its author wrote, where a parse
    /// failure would have taken the whole tool result with it.
    #[test]
    fn a_display_of_a_kind_this_surface_never_learned_prints_its_fold() {
        let display: bingo_sdk::View = serde_json::from_value(serde_json::json!({
            "kind": "chart.candles",
            "series": [1, 2],
            "fold": "AAPL 1 2",
        }))
        .expect("a word from a newer speaker is text, not an error");
        let output = ToolOutput {
            display: Some(display),
            ..ToolOutput::text("charted AAPL")
        };
        let frames = vec![frame(
            1,
            Event::ItemCompleted {
                item: tool_call("itm_2", "Chart", Some(output), ItemStatus::Completed),
            },
        )];
        assert_eq!(
            play(Mode::Text, &frames).err(),
            "[tool] Chart ok (12ms)\n  AAPL 1 2\n"
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
    fn the_two_registers_of_a_report() {
        assert_eq!(
            error_report(ErrorCode::AuthRequired, "No key.\nSet one.", false),
            "[error] code=AUTH_REQUIRED msg=No key. Set one."
        );
        assert_eq!(
            error_report(ErrorCode::AuthRequired, "No key.", true),
            "error: No key."
        );
        assert_eq!(
            notice_report("UNKNOWN_SETTING", "unknown `theme`", false),
            "[notice] UNKNOWN_SETTING unknown `theme`"
        );
        assert_eq!(
            notice_report("UNKNOWN_SETTING", "unknown `theme`", true),
            "note: unknown `theme`"
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
        let line = error_report(ErrorCode::Internal, &"x".repeat(500), false);
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
            Mode::from_args(&serde_json::json!({"outputFormat": "stream-json"})),
            Mode::StreamJson
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
    fn text_and_json_report_the_root_alone_and_the_envelope_the_tree() {
        let root = session_state();
        let mut child = session_state();
        child.summary.id = SessionId::from_raw("ses_2");
        let mut said = frame(
            1,
            Event::ItemCompleted {
                item: assistant("itm_1", "from the child", ItemStatus::Completed),
            },
        );
        said.session = SessionId::from_raw("ses_2");
        for mode in [Mode::Text, Mode::Json, Mode::StreamJson] {
            let mut renderer = Renderer::new(mode, false, Vec::new());
            let (mut out, mut err) = (Vec::new(), Vec::new());
            renderer
                .render(&said, &child, &root, &mut out, &mut err)
                .expect("writing to a vector cannot fail");
            assert_eq!(
                !out.is_empty(),
                mode == Mode::StreamJson,
                "{mode:?} wrote {:?}",
                String::from_utf8_lossy(&out)
            );
            assert!(err.is_empty());
        }
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
    #[test]
    fn a_commands_receipt_lands_on_stdout_as_its_message_or_its_fold() {
        let frames = vec![
            frame(
                1,
                Event::IntentAck {
                    intent: bingo_sdk::IntentId::from_raw("req_1"),
                    outcome: IntentOutcome::Applied {
                        result: serde_json::json!({"message": "model: fake/one"}),
                    },
                },
            ),
            frame(
                2,
                Event::IntentAck {
                    intent: bingo_sdk::IntentId::from_raw("req_2"),
                    outcome: IntentOutcome::Applied {
                        result: serde_json::json!({"view": {"kind": "keyValue", "rows": [["mode", "default"]]}}),
                    },
                },
            ),
            frame(
                3,
                Event::IntentAck {
                    intent: bingo_sdk::IntentId::from_raw("req_3"),
                    outcome: IntentOutcome::Applied {
                        result: serde_json::json!({"item": "itm_1"}),
                    },
                },
            ),
        ];
        let sinks = play(Mode::Text, &frames);
        assert_eq!(sinks.out(), "model: fake/one\nmode: default\n");
        assert_eq!(sinks.err(), "", "a receipt is an answer, not a diagnostic");
    }
}
