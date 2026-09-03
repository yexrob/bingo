//! One item to styled lines, in Claude Code's grammar (`docs/design/tui.md`
//! §4): `⏺` for what the model says and does, `⎿` for what came back, `>` on a
//! raised bar for what you said. The reducer is the only history: nothing here
//! remembers a thing, and [`crate::blocks`] stacks and memoises what it draws.

use std::time::{Duration, Instant};

use bingo_sdk::{
    CommandSpec, ContentPart, DecisionKind, Item, ItemBody, ItemStatus, Origin, SessionState,
    ToolOutput, TurnStatus, View,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::clock::{self, Anim, Now};
use crate::fold::{self, Fold, Folds};
use crate::skill::{self, Run};
use crate::tree::{self, Agents};
use crate::{acp, commands, markdown, paths, theme, views, wrap};

/// How long the comet tail of a block still arriving takes to cool (§6).
pub const COMET: Duration = Duration::from_millis(180);
/// How many cells of it are still warm.
const COMET_CELLS: usize = 8;
/// One pulse of a live tool's bullet (§6).
const PULSE: Duration = Duration::from_millis(1200);

/// Output rows kept under a finished tool row before the rest folds away.
const OUTPUT_ROWS: usize = 5;
/// A running tool's tail: enough to see it move, few enough to look past.
const TAIL_ROWS: usize = 3;
/// A thought's rows, streaming or peeked at. Fewer than a tool's tail, and the
/// reason is the difference between the two: a tool's output is *read* — a
/// person is looking for the line that says what happened — and a thought is
/// read *past*. Two rows are enough to see one moving and to recognise where
/// it went; a third would spend a row of the transcript on working nobody
/// asked for.
const THOUGHT_ROWS: usize = 2;
/// Diff rows kept under a tool row.
const DIFF_ROWS: usize = 12;
/// What opens a folded result. The key is the frame's; the words are here
/// because this is what is folded.
const EXPAND: &str = "ctrl+o to expand";

/// What every row of one transcript needs to know about where it is.
pub struct Rows<'a> {
    pub cwd: &'a str,
    pub width: usize,
    /// How much of each block a person opened or shut; everything else wears
    /// its kind's own start ([`crate::fold`]).
    pub folds: &'a Folds,
    /// The commands catalogue the surface read from the host, which is where a
    /// `/name` says what family it belongs to — the one way a row can tell a
    /// skill from another command that answers with a prompt
    /// ([`crate::skill`]).
    pub commands: &'a [CommandSpec],
    /// The frame being drawn: what every cue below is a function of.
    pub now: Now,
    /// What the session in view is called. A post says which room it came
    /// from, and the room's own transcript is the one place that says nothing.
    pub title: Option<&'a str>,
}

impl<'a> Rows<'a> {
    /// One frame's worth of where a row is: the session in view names itself,
    /// the surface supplies the room it has, what a person opened, the
    /// catalogue and the clock.
    pub fn of(
        state: &'a SessionState,
        width: usize,
        folds: &'a Folds,
        commands: &'a [CommandSpec],
        now: Now,
    ) -> Self {
        Self {
            cwd: &state.summary.cwd,
            width,
            folds,
            commands,
            now,
            title: state.summary.title.as_deref(),
        }
    }
}

/// Where one block is in its own motion (§6): the clock [`crate::blocks`]
/// measured for it, and whether this is the one frame its completion flashes
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cue {
    pub since: Instant,
    pub flip: bool,
}

impl Rows<'_> {
    /// Prose is read, not scanned: it stops at the measure (design §7).
    fn measure(&self) -> usize {
        wrap::measure(self.width)
    }

    /// The cells a result has, once the `⎿` gutter has taken its own.
    fn result_width(&self) -> usize {
        self.measure().saturating_sub(connector().width()).max(1)
    }

    /// A path as a person reads it, from this session's own directory.
    fn shorten(&self, text: &str) -> String {
        paths::shorten_in(text, self.cwd, paths::home())
    }
}

/// A receipt is the answer to the row above it, so it opens no block of its
/// own (design §4: the receipt joins the result).
pub fn joins_the_row_above(item: &Item) -> bool {
    matches!(item.body, ItemBody::PermissionReceipt { .. })
}

/// One item's block. `previous` is the item before it, which a receipt joins;
/// `agents` the sub-sessions this transcript's calls spawned.
pub fn item_lines(
    item: &Item,
    previous: Option<&Item>,
    agents: &Agents<'_>,
    rows: &Rows<'_>,
    cue: Cue,
) -> Vec<Line<'static>> {
    // How much of this block is on the screen — what `ctrl+o` and a click both
    // write, over the start its kind has. One question, asked once, here.
    let fold = fold::fold_of(rows.folds, item);
    match &item.body {
        ItemBody::User { parts, origin } => match quiet(origin) {
            true => notice(item, parts, origin, fold, rows),
            false => user(parts, origin.principal.as_deref(), rows),
        },
        ItemBody::Assistant { text } => assistant(text, item.status, rows, cue),
        // An agent that runs its own tools journals each finished call as a
        // reasoning item whose metadata is the whole call (ADR-0035 §4), so
        // what a reasoning item draws is the one question of whether it is a
        // thought or a call ([`crate::acp`]).
        ItemBody::Reasoning { .. } => match acp::call(item) {
            Some(call) => agent_call(call, fold, rows, cue),
            None => thinking(item, fold, rows),
        },
        ItemBody::ToolCall { .. } => called(item, agents, fold, rows, cue),
        ItemBody::Action { name, args, result } => {
            action(item.status, name, args, result.as_ref(), fold, rows)
        }
        ItemBody::Compaction { before, after, .. } => vec![rule(
            &format!("context compacted ({before} → {after} tokens)"),
            rows.width,
        )],
        ItemBody::Rewind { dropped, .. } => {
            vec![rule(
                &format!("rewound, {dropped} items dropped"),
                rows.width,
            )]
        }
        ItemBody::Interruption { marker } => {
            vec![Line::from(Span::styled(marker.clone(), theme::dim()))]
        }
        ItemBody::Notice { level, text, .. } => {
            vec![Line::from(Span::styled(text.clone(), theme::level(*level)))]
        }
        ItemBody::QuestionAnswer {
            question, answer, ..
        } => answered(question, answer, rows),
        ItemBody::PermissionReceipt {
            tool,
            decision,
            feedback,
            ..
        } => receipt(tool, *decision, feedback.as_deref(), previous, rows),
        ItemBody::Asset { asset, label } => vec![Line::from(Span::styled(
            format!("[{}]", label.clone().unwrap_or_else(|| asset.clone())),
            theme::dim(),
        ))],
    }
}

/// A call that started a session is that session's row; every other call is
/// its own (design §3: a child is a row where it began).
fn called(
    item: &Item,
    agents: &Agents<'_>,
    fold: Fold,
    rows: &Rows<'_>,
    cue: Cue,
) -> Vec<Line<'static>> {
    let ItemBody::ToolCall {
        name,
        input,
        output,
        progress,
        ..
    } = &item.body
    else {
        return Vec::new();
    };
    match agents.get(&item.id) {
        Some(child) => child_row(child, input, rows),
        None => tool_call(
            Call {
                status: item.status,
                name,
                about: summarize(input),
                output: output.as_ref(),
                progress: progress.as_deref(),
                fold,
                run: skill::of(item, rows.commands),
            },
            rows,
            cue,
        ),
    }
}

/// One of the agent's own calls, as the row every other call wears: the same
/// bullet, the same signature, the same folded result. The heading lines the
/// provider wrote into the item's text are what this row replaces — drawn as
/// well as the row, the two would say the same thing twice.
///
/// A call still running has no metadata to read (the provider writes it when
/// the block closes), so it never reaches here: it draws as the thought it
/// looks like, which is what it did before this row existed.
fn agent_call(call: acp::Call, fold: Fold, rows: &Rows<'_>, cue: Cue) -> Vec<Line<'static>> {
    let acp::Call {
        name,
        about,
        status,
        output,
    } = call;
    tool_call(
        Call {
            status,
            name,
            about,
            output: output.as_ref(),
            progress: None,
            fold,
            run: None,
        },
        rows,
        cue,
    )
}

// ---- the two marks ------------------------------------------------------

/// Where the text under a `⏺` starts: column 2 (design §4).
fn speaks_indent() -> usize {
    theme::bullet().width() + 1
}

/// The `  ⎿  ` a result hangs from, and where its text starts: column 5.
fn connector() -> String {
    format!("  {}  ", theme::connector())
}

/// A block under a mark: the mark on its first row, its text at `indent` on
/// every row under it, wrapped so nothing overflows the measure.
fn under(
    mark: Span<'static>,
    body: Vec<Line<'static>>,
    indent: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(indent).max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in wrap::wrap_all(&body, inner) {
        let lead = match out.is_empty() {
            true => mark.clone(),
            false => Span::raw(" ".repeat(indent)),
        };
        let mut spans = vec![lead];
        spans.extend(line.spans);
        out.push(Line::from(spans));
    }
    out
}

/// What the model says and does.
fn speaks(style: Style, body: Vec<Line<'static>>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    marked(theme::bullet(), style, body, rows)
}

/// A block under a glyph in the bullet's place. The glyph says what kind of
/// row it is and the style says what state it is in, which is why the two are
/// separate: a skill's mark still turns `good` when its call comes back.
fn marked(
    glyph: &str,
    style: Style,
    body: Vec<Line<'static>>,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let mark = Span::styled(format!("{glyph} "), style);
    under(mark, body, speaks_indent(), rows.measure())
}

/// What came back.
fn returns(body: Vec<Line<'static>>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mark = connector();
    let indent = mark.width();
    under(
        Span::styled(mark, theme::dim()),
        body,
        indent,
        rows.measure(),
    )
}

// ---- the kinds ----------------------------------------------------------

/// The surfaces whose input is the machinery reporting in rather than
/// somebody speaking: a background job that ended, a message from another
/// session, a room's post, a scheduled turn, a command's own prompt. What they
/// deliver reads as a tool row does, because that is what it is — something
/// that happened, not something anyone said to you.
///
/// `command` is the one a person did set in motion, and it is here anyway: a
/// `/guide` puts a page of skill body in the journal under a line nobody
/// typed, and drawn as prose it is a wall of somebody else's words on the
/// person's own bar. It reads as the machinery it is, and where the command
/// was a skill the row is the run itself — `❖ Skill(guide) …`, the same row
/// the model's own way to that body draws ([`skill_row`]).
///
/// The set is closed, and this list is the only place it is written down: a
/// surface nobody has put here is loud. A new subsystem chooses its side by
/// being added or left out, deliberately, and the cost of each mistake says
/// which way to lean — a person's own words drawn as machinery is a wrong
/// nobody can undo by reading harder.
const QUIET_SURFACES: &[&str] = &["agent", "bash", commands::SURFACE, "room", "schedule"];

/// Whether a delivery is the machinery reporting in. The composer's pending
/// area asks the same question of what is still queued (ADR-0028), so the set
/// stays one list read from two places rather than two lists to keep in step.
pub(crate) fn quiet(origin: &Origin) -> bool {
    QUIET_SURFACES.contains(&origin.surface.as_str())
}

/// What a `User` item says, as its parts spell it.
fn said(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("")
}

/// A subsystem's notice, marked the way a tool row is: the bullet, the one
/// line that says what happened, and the rest of it hanging under a `⎿` —
/// dim, folded, subordinate. The text already leads with the outcome, so the
/// first line is the summary and nothing has to be invented for it.
fn notice(
    item: &Item,
    parts: &[ContentPart],
    origin: &Origin,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let text = said(parts);
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
    // A blank line between the headline and the rest is a separator in the
    // text — a command's prompt puts one there — and under a `⎿` it would be
    // a row spent on nothing.
    let rest = rest.trim_start_matches('\n');
    // A skill is one row however it was asked for, so the typed line gives the
    // headline up to the run's own signature. The line itself stays in the
    // item, which is where a rewind reads it back.
    let style = bullet_style(item.status, false);
    let mut out = match skill::of(item, rows.commands) {
        Some(run) => skill_row(run, style, rows),
        None => speaks(
            style,
            vec![headline(
                head,
                origin.principal.as_deref(),
                elsewhere(origin, rows),
            )],
            rows,
        ),
    };
    if !rest.trim().is_empty() {
        out.extend(returns(kept(plain(rest), fold, OUTPUT_ROWS, None), rows));
    }
    out
}

/// The row one skill run wears, whichever door it came through: the model's
/// `Skill(guide)` call and a person's own `/guide` are the same thing
/// happening, so this is the only place either is drawn (design §4).
///
/// The mark is the skill's own glyph in the bullet's place, in the colour the
/// bullet would have worn: what kind of row it is and what state it is in are
/// two facts, and each keeps its own carrier.
fn skill_row(run: Run<'_>, style: Style, rows: &Rows<'_>) -> Vec<Line<'static>> {
    marked(theme::skill(), style, vec![asked(run, rows)], rows)
}

/// `Skill(guide) the wire format`: the call as any tool row spells one, then
/// the free text the skill was given — outside the parentheses, because it is
/// what the skill reads and not what it is.
fn asked(run: Run<'_>, rows: &Rows<'_>) -> Line<'static> {
    let mut line = signature(skill::TOOL, run.name, rows);
    if !run.args.is_empty() {
        line.spans
            .push(Span::styled(format!(" {}", run.args), theme::text()));
    }
    line
}

/// The conversation a delivery says it came from, where saying it tells a
/// person something: in a member's own transcript a room post is one of
/// several conversations arriving, and in the room's own it is the only one.
fn elsewhere<'a>(origin: &'a Origin, rows: &Rows<'_>) -> Option<&'a str> {
    origin
        .conversation
        .as_deref()
        .filter(|room| Some(*room) != rows.title)
}

/// The marked line itself: the sender's name where the origin carries one —
/// an agent, a room's member — where they said it, and what happened.
fn headline(head: &str, principal: Option<&str>, conversation: Option<&str>) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(name) = principal {
        let bold = theme::text().patch(theme::bold());
        spans.push(Span::styled(name.to_string(), bold));
        match conversation {
            // Who is bold, where is furniture: the room is dim so the name
            // still wins the row (design §2).
            Some(room) => spans.push(Span::styled(format!(" in {room}: "), theme::dim())),
            None => spans.push(Span::styled(": ".to_string(), bold)),
        }
    }
    spans.push(Span::styled(head.to_string(), theme::text()));
    Line::from(spans)
}

/// A person's own line, on a bar the width of the transcript. An origin that
/// names a principal is somebody else speaking — a channel's correspondent, a
/// person writing from elsewhere — so the line says who, as a chat does. Where
/// they said it is the view one is looking at; saying it again would be noise.
fn user(parts: &[ContentPart], principal: Option<&str>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let text = said(parts);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut body: Vec<Line<'static>> = text
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::text())))
        .collect();
    if let Some(name) = principal
        && let Some(first) = body.first_mut()
    {
        first.spans.insert(
            0,
            Span::styled(format!("{name}: "), theme::text().patch(theme::bold())),
        );
    }
    let mark = Span::styled(format!("{} ", theme::user()), theme::dim());
    under(mark, body, speaks_indent(), rows.measure())
        .into_iter()
        .map(|line| bar(line, rows.width))
        .collect()
}

/// The raised bar behind a `>` line: it runs to the edge of the transcript,
/// so what you said is a band and not a sentence.
fn bar(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = line.spans;
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    let mut line = Line::from(spans);
    line.style = theme::raised();
    line
}

/// The answer: the brightest text on the screen, after a white `⏺` — and,
/// while it is still arriving, a comet tail on the cells that just landed.
fn assistant(text: &str, status: ItemStatus, rows: &Rows<'_>, cue: Cue) -> Vec<Line<'static>> {
    let body = markdown::render(text, rows.measure().saturating_sub(speaks_indent()));
    let body = match arriving(status, rows, cue) {
        Some(age) => comet(body, age),
        None => body,
    };
    speaks(theme::text().patch(theme::bold()), body, rows)
}

/// How far a block still arriving is through its tail, and nothing at all
/// once the tail has cooled or where nothing may move.
fn arriving(status: ItemStatus, rows: &Rows<'_>, cue: Cue) -> Option<f32> {
    if status != ItemStatus::Running || !rows.now.motion {
        return None;
    }
    let age = Anim::new(cue.since, COMET).progress(rows.now.instant);
    (age < 1.0).then_some(age)
}

/// The comet tail (§6): the cells that just arrived wear `presence`'s glow and
/// cool to `text` behind them. It is a style pass over the last row — the text
/// is the reducer's, and nothing here keeps a copy of it to know what is new.
fn comet(mut body: Vec<Line<'static>>, age: f32) -> Vec<Line<'static>> {
    let Some(last) = body.pop() else {
        return body;
    };
    body.push(tail_lit(last, age));
    body
}

fn tail_lit(line: Line<'static>, age: f32) -> Line<'static> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let from = total.saturating_sub(COMET_CELLS);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut seen = 0;
    for span in line.spans {
        let count = span.content.chars().count();
        if seen + count <= from {
            seen += count;
            out.push(span);
            continue;
        }
        let keeps = from.saturating_sub(seen);
        let head: String = span.content.chars().take(keeps).collect();
        if !head.is_empty() {
            out.push(Span::styled(head, span.style));
        }
        for (i, c) in span.content.chars().enumerate().skip(keeps) {
            out.push(Span::styled(
                c.to_string(),
                cooling(total - (seen + i) - 1, age),
            ));
        }
        seen += count;
    }
    Line::from(out)
}

/// One cell of the tail: the newest is the warmest, and each cell behind it is
/// that much further along the same ramp.
fn cooling(back: usize, age: f32) -> Style {
    let behind = back as f32 / COMET_CELLS as f32;
    theme::comet((age + behind).min(1.0))
}

/// A thought is readable where it happened, and while it is being had: the
/// row says `✻ Thinking…` over the newest two rows of what has arrived, then
/// closes to `✻ Thought for 2s` alone.
///
/// One match on one fact — whether the thinking is over — because that is the
/// only thing the two halves differ by.
fn thinking(item: &Item, fold: Fold, rows: &Rows<'_>) -> Vec<Line<'static>> {
    match item.completed_at {
        None => still_thinking(item, fold, rows),
        Some(end) => thought_for(item, end, fold, rows),
    }
}

/// A thought as it is being had: the row, and under it the newest
/// [`THOUGHT_ROWS`] of what has been thought so far — dim under the same `⎿`
/// a running tool's tail hangs from (§6), scrolling up as the deltas arrive.
///
/// They wear no comet tail. The comet is `presence`'s glow on words being said
/// (§6 "streaming"), and thinking is where `dim` lives (§4): a second warm
/// light beside the sparkle would put motion on the one thing a person is
/// meant to read past.
fn still_thinking(item: &Item, fold: Fold, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mut out = vec![sparkled(
        format!("Thinking{}", theme::ellipsis()),
        theme::dim().patch(theme::italic()),
    )];
    if let Some(text) = thought(item) {
        out.extend(returns(streaming(text, fold), rows));
    }
    out
}

/// What is under a thought still being had: the newest rows of it, which is
/// the only cut that can follow something that grows from the bottom; the
/// whole of it where a person opened it; nothing where they shut it.
///
/// It carries no `… +N lines`: the count would change under the reader on
/// every delta, and a tail is not a promise that the rest is reachable — that
/// is what the row a thought decays to is for.
fn streaming(text: &str, fold: Fold) -> Vec<Line<'static>> {
    match fold {
        Fold::Shut => Vec::new(),
        Fold::Peek => tail(text, THOUGHT_ROWS),
        Fold::Open => plain(text),
    }
}

/// A thought that is over: how long it took, and — where a person has asked
/// for it — what was thought, dim under the `⎿`.
///
/// The row is alone by default. A finished thought is working, not an answer:
/// it is read past, and the rows it would spend belong to what came of it. The
/// text is one click or one `ctrl+o` away, and the peek starts at the *top*,
/// because somebody opening a finished thought is reading it from the
/// beginning rather than watching it move.
fn thought_for(
    item: &Item,
    end: jiff::Timestamp,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let mut out = vec![sparkled(
        format!("Thought for {}", took(end.duration_since(item.started_at))),
        theme::dim(),
    )];
    if let Some(text) = thought(item) {
        out.extend(returns(
            kept(plain(text), fold, THOUGHT_ROWS, Some(EXPAND)),
            rows,
        ));
    }
    out
}

/// The `✻` and what it says beside it.
fn sparkled(text: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", theme::spark()), theme::dim()),
        Span::styled(text, style),
    ])
}

/// How long a thought took, as a person reads a clock: something that happened
/// took some time, so under a second is `<1s` and never `0s`.
fn took(span: jiff::SignedDuration) -> String {
    match span.as_secs() {
        seconds if seconds < 1 => "<1s".to_string(),
        seconds => format!("{seconds}s"),
    }
}

/// What a thought has under it: what was thought. `None` for one that came
/// back empty — Anthropic's redacted thinking, an OpenAI turn the provider
/// summarised nothing of — which draws the row alone, folds nothing, opens
/// nothing and so promises nothing.
pub fn thought(item: &Item) -> Option<&str> {
    match &item.body {
        ItemBody::Reasoning { text, .. } if !text.trim().is_empty() => Some(text),
        _ => None,
    }
}

/// One tool call, as much of it as there is yet.
struct Call<'a> {
    status: ItemStatus,
    name: &'a str,
    /// What the row says the call is about, in the words whoever built it
    /// reads the call in: a tool's input as [`summarize`] spells it, an ACP
    /// agent's own input or title ([`crate::acp`]).
    about: String,
    output: Option<&'a ToolOutput>,
    progress: Option<&'a str>,
    /// How much of what came back is shown.
    fold: Fold,
    /// The skill this call is, when it is one: the row is the run's, not the
    /// tool's, and what came back still hangs under it.
    run: Option<Run<'a>>,
}

fn tool_call(call: Call<'_>, rows: &Rows<'_>, cue: Cue) -> Vec<Line<'static>> {
    let failed = call.status == ItemStatus::Failed || call.output.is_some_and(|o| o.is_error);
    let style = live_bullet(call.status, failed, rows, cue);
    let mut out = match call.run {
        Some(run) => skill_row(run, style, rows),
        None => speaks(style, vec![signature(call.name, &call.about, rows)], rows),
    };
    out.extend(result(&call, rows));
    out
}

/// The bullet says what state the row is in; its motion says how fresh that
/// state is — it pulses between `presence` and its glow while the tool runs,
/// and flashes bold for one frame as the answer lands (§6).
fn live_bullet(status: ItemStatus, failed: bool, rows: &Rows<'_>, cue: Cue) -> Style {
    let settled = bullet_style(status, failed);
    if !rows.now.motion {
        return settled;
    }
    if cue.flip {
        return settled.patch(theme::bold());
    }
    match status {
        ItemStatus::Running => theme::pulse(clock::breath(rows.now, PULSE)),
        _ => settled,
    }
}

/// `Read(Cargo.toml)`: the name bold, what it is about plain.
fn signature(name: &str, about: &str, rows: &Rows<'_>) -> Line<'static> {
    let mut spans = vec![Span::styled(name.to_string(), theme::bold())];
    let about = rows.shorten(about);
    if !about.is_empty() {
        spans.push(Span::styled(format!("({about})"), theme::text()));
    }
    Line::from(spans)
}

/// The bullet carries the state, and nothing else has to.
fn bullet_style(status: ItemStatus, failed: bool) -> Style {
    if failed {
        return theme::bad();
    }
    match status {
        ItemStatus::Running => theme::presence(),
        ItemStatus::Completed => theme::good(),
        ItemStatus::Failed => theme::bad(),
        ItemStatus::Pending | ItemStatus::Interrupted => theme::dim(),
    }
}

/// What is under a tool row: its tail while it runs, its output once it is
/// done, folded to a line that says how much was left out.
fn result(call: &Call<'_>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    if call.status == ItemStatus::Running {
        let Some(progress) = call.progress else {
            return Vec::new();
        };
        return returns(tail(progress, TAIL_ROWS), rows);
    }
    let Some(output) = call.output else {
        return Vec::new();
    };
    returns(folded(output, call.fold, rows.result_width()), rows)
}

/// The last `keep` rows of something still arriving: what a running tool has
/// printed so far, or what a thought has been thinking. One tail, so the two
/// move the same way (§6); how many rows each spends is the one thing they
/// differ by, and each names its own.
fn tail(arriving: &str, keep: usize) -> Vec<Line<'static>> {
    let all: Vec<&str> = arriving.trim_end().lines().collect();
    plain(&all[all.len().saturating_sub(keep)..].join("\n"))
}

/// What a person reads under a finished tool row: the display the tool drew
/// for them when there is one (ADR-0013 §2, the block lane), else the text the
/// model read, folded to what a row can spare either way.
fn folded(output: &ToolOutput, fold: Fold, width: usize) -> Vec<Line<'static>> {
    let (rows, limit) = match &output.display {
        // A diff is the one display a person reads by the dozen rows.
        Some(view @ View::Diff { .. }) => (views::render(view, width), DIFF_ROWS),
        Some(view) => (views::render(view, width), OUTPUT_ROWS),
        None => (plain(&text_of(output)), OUTPUT_ROWS),
    };
    kept(rows, fold, limit, Some(EXPAND))
}

/// Everything a result says, with nothing folded away: what the pager opens
/// (design §5 — a long output opens in a sheet).
pub fn whole(output: &ToolOutput, width: usize) -> Vec<Line<'static>> {
    folded(output, Fold::Open, width)
}

fn plain(text: &str) -> Vec<Line<'static>> {
    text.trim_end()
        .lines()
        .map(|line| Line::from(Span::styled(expand_tabs(line), theme::dim())))
        .collect()
}

/// A terminal cell has no tab in it: each one runs to the next stop of eight,
/// as the shell would have shown it.
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut column = 0;
    for c in line.chars() {
        if c == '\t' {
            let stop = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', stop));
            column += stop;
        } else {
            out.push(c);
            column += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    out
}

fn text_of(output: &ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a block shows under its row, from the one fold it is in: nothing, the
/// first `limit` rows with how many were left out, or the whole of it. One map
/// answers for every fold, so a block is open in one way only.
fn kept(
    rows: Vec<Line<'static>>,
    fold: Fold,
    limit: usize,
    opens: Option<&str>,
) -> Vec<Line<'static>> {
    match fold {
        Fold::Shut => Vec::new(),
        Fold::Peek => cut(rows, limit, opens),
        Fold::Open => rows,
    }
}

/// The first rows, then how many were left out and what opens them. `opens` is
/// `None` for what no key reaches: `ctrl+o` reaches a result, so a block that
/// is not one says how much it kept back and promises nothing.
fn cut(rows: Vec<Line<'static>>, limit: usize, opens: Option<&str>) -> Vec<Line<'static>> {
    let hidden = rows.len().saturating_sub(limit);
    let mut out: Vec<Line<'static>> = rows.into_iter().take(limit).collect();
    if hidden > 0 {
        let key = opens.map(|key| format!(" ({key})")).unwrap_or_default();
        out.push(Line::from(Span::styled(
            format!("{} +{hidden} lines{key}", theme::ellipsis()),
            theme::dim(),
        )));
    }
    out
}

/// A sub-session is a row where it began: what it is, what it was asked, and
/// what it is doing — read from its own state, never copied into this one.
fn child_row(child: &SessionState, input: &Value, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mut out = speaks(
        tree::bullet_style(tree::Status::of(child), tree::asking(child)),
        vec![signature(&tree::name(child), &summarize(input), rows)],
        rows,
    );
    if let Some(activity) = tree::activity(child) {
        // A child that is waiting on a person pulses until they go to it;
        // one that is simply working recedes like every other result.
        let style = match tree::asking(child) {
            true => theme::attention(rows.now),
            false => theme::dim(),
        };
        out.extend(returns(
            vec![Line::from(Span::styled(activity, style))],
            rows,
        ));
    }
    out
}

/// What the call is about, from the field a person would recognise.
pub fn summarize(input: &Value) -> String {
    for key in ["file_path", "command", "pattern", "url", "query", "prompt"] {
        if let Some(Value::String(value)) = input.get(key) {
            return value.clone();
        }
    }
    match input {
        Value::Object(map) => map
            .iter()
            .find_map(|(_, v)| v.as_str())
            .unwrap_or_default()
            .to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A long-running operation of the surface's own: login, reconnect, a team
/// starting. It reads as a tool row because that is what it is.
fn action(
    status: ItemStatus,
    name: &str,
    args: &Value,
    result: Option<&Value>,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let mut out = speaks(
        bullet_style(status, false),
        vec![signature(name, &as_text(args), rows)],
        rows,
    );
    if let Some(result) = result {
        out.extend(returns(
            kept(plain(&as_text(result)), fold, OUTPUT_ROWS, Some(EXPAND)),
            rows,
        ));
    }
    out
}

/// Strings travel verbatim; anything else as compact JSON.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A question the model asked and the answer it was given.
fn answered(question: &str, answer: &str, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mut out = speaks(
        theme::text().patch(theme::bold()),
        vec![Line::from(Span::styled(
            question.to_string(),
            theme::text(),
        ))],
        rows,
    );
    out.extend(returns(
        vec![Line::from(Span::styled(answer.to_string(), theme::dim()))],
        rows,
    ));
    out
}

/// The gate's answer, joined to the row that asked for it. The tool is named
/// only when the row above did not already name it.
fn receipt(
    tool: &str,
    decision: DecisionKind,
    feedback: Option<&str>,
    previous: Option<&Item>,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let verdict = match decision {
        DecisionKind::Allow => "allowed",
        DecisionKind::AllowSession => "allowed for this session",
        DecisionKind::Deny => "denied",
    };
    let said = match previous.is_some_and(|item| calls(item, tool)) {
        true => verdict.to_string(),
        false => format!("{tool} {verdict}"),
    };
    let text = match feedback {
        Some(feedback) => format!("{said} — {}", rows.shorten(feedback)),
        None => said,
    };
    returns(vec![Line::from(Span::styled(text, theme::dim()))], rows)
}

fn calls(item: &Item, tool: &str) -> bool {
    matches!(&item.body, ItemBody::ToolCall { name, .. } if name == tool)
}

/// A turn that failed says why on a `⏺` of its own, derived from `last_turn`
/// rather than kept as a line of the surface's own. It belongs to no item, so
/// it is the transcript's last block rather than one of them.
pub fn failure(state: &SessionState, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let Some(TurnStatus::Failed { error }) = state.last_turn.as_ref().filter(|_| !state.busy())
    else {
        return Vec::new();
    };
    speaks(
        theme::bad(),
        vec![Line::from(Span::styled(
            error.message.clone(),
            theme::bad(),
        ))],
        rows,
    )
}

/// A full-width divider with its reason in the middle of the left run.
fn rule(text: &str, width: usize) -> Line<'static> {
    let head = format!("{0}{0}{0} {text} ", theme::rule());
    let tail = width.saturating_sub(head.width());
    Line::from(Span::styled(
        format!("{head}{}", theme::rule().repeat(tail)),
        theme::dim(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        agent_call, assistant, completed, delivered, folded, frame, item, post, receipt_item,
        running_tool, scene, started, started_tool, tool, ts, user as person,
    };
    use bingo_sdk::{Event, ItemId};

    fn drawn(items: Vec<Item>) -> Vec<String> {
        drawn_in(None, items)
    }

    /// The same items in a session of that name: a room's own transcript when
    /// the name is the room's.
    fn drawn_in(title: Option<&str>, items: Vec<Item>) -> Vec<String> {
        let mut state = stated(items);
        state.summary.title = title.map(str::to_string);
        rendered(&state)
    }

    /// The same items in a surface whose catalogue files `guide` under the
    /// skills, which is how a `/guide` line is known to be one.
    fn drawn_knowing_the_skill(items: Vec<Item>) -> Vec<String> {
        rendered_with(&stated(items), &Folds::new(), &catalogue())
    }

    fn stated(items: Vec<Item>) -> SessionState {
        folded(
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| frame(i as u64 + 1, Event::ItemCompleted { item }))
                .collect(),
        )
    }

    fn catalogue() -> Vec<CommandSpec> {
        vec![CommandSpec {
            name: "guide".into(),
            aliases: Vec::new(),
            hint: String::new(),
            args: bingo_sdk::ArgSpec::Free {
                hint: String::new(),
            },
            instant: false,
            family: "skill".into(),
        }]
    }

    /// The transcript without the welcome box, which `welcome.rs` pins on its
    /// own: these tests are about the grammar under it.
    fn rendered(state: &SessionState) -> Vec<String> {
        rendered_with(state, &Folds::new(), &[])
    }

    fn rendered_with(state: &SessionState, folds: &Folds, commands: &[CommandSpec]) -> Vec<String> {
        let welcomed = crate::welcome::lines(state, 60).len();
        let mut blocks = crate::blocks::Blocks::default();
        let rows = Rows::of(state, 60, folds, commands, scene().1);
        let height = blocks.sync(state, &Agents::new(), &rows, Vec::new());
        let mut rows: Vec<String> = blocks
            .window(0, height)
            .iter()
            .skip(welcomed)
            .map(|line| line.to_string().trim_end().to_string())
            .collect();
        if rows.first().is_some_and(String::is_empty) {
            rows.remove(0);
        }
        rows
    }

    #[test]
    fn a_post_says_who_wrote_it_and_a_persons_own_line_does_not() {
        assert_eq!(
            drawn(vec![
                post("itm_1", "reviewer", "two nits, otherwise fine"),
                person("itm_2", "thanks"),
            ]),
            vec![
                "⏺ reviewer in #design: two nits, otherwise fine".to_string(),
                String::new(),
                "> thanks".to_string(),
            ],
            "the machinery is marked; what a person typed is a block"
        );
    }

    /// Where a post is read decides whether it says where it came from: a
    /// member's own transcript carries every conversation it is in, and a
    /// room's carries one.
    #[test]
    fn a_post_names_its_room_everywhere_but_in_the_room() {
        let post = || vec![post("itm_1", "scout", "found it")];
        assert_eq!(drawn(post()), vec!["⏺ scout in #design: found it"]);
        assert_eq!(
            drawn_in(Some("#design"), post()),
            vec!["⏺ scout: found it"],
            "in the room's own transcript the name stands alone"
        );
    }

    #[test]
    fn a_second_line_of_yours_stays_on_the_bar_under_the_first() {
        assert_eq!(
            drawn(vec![person("itm_1", "one\ntwo")]),
            vec!["> one".to_string(), "  two".to_string()],
        );
    }

    /// The whole of the closed set, read from the list itself, and a handful
    /// of surfaces outside it — the ones that exist and one that never will.
    #[test]
    fn every_quiet_surface_is_a_marked_line_and_nothing_else_is() {
        for surface in QUIET_SURFACES {
            assert_eq!(
                drawn(vec![delivered("itm_1", surface, None, "it ended")]),
                vec!["⏺ it ended".to_string()],
                "{surface} is quiet"
            );
        }
        for loud in ["tui", "print", "rpc", "acp", "channels", "brand-new"] {
            assert_eq!(
                drawn(vec![delivered("itm_1", loud, None, "it ended")]),
                vec!["> it ended".to_string()],
                "{loud} is not in the set, so it stays loud"
            );
        }
    }

    /// A job reporting in: the first line says what happened, and what one
    /// does about it hangs under the row the way a result does.
    #[test]
    fn a_finished_job_reads_as_a_tool_row_does() {
        assert_eq!(
            drawn(vec![delivered(
                "itm_1",
                "bash",
                None,
                "Background job ab12cd34 exited with code 1 after 2m.\n\
                 `BashOutput` with id \"ab12cd34\" reads what it wrote.",
            )]),
            vec![
                "⏺ Background job ab12cd34 exited with code 1 after 2m.".to_string(),
                "  ⎿  `BashOutput` with id \"ab12cd34\" reads what it wrote.".to_string(),
            ],
        );
    }

    /// A skill's prompt as the kernel journals it: the typed line, then the
    /// body the command expanded to.
    fn skill_prompt() -> Item {
        delivered(
            "itm_1",
            "command",
            None,
            "/guide the wire format\n\n\
             Base directory for this skill: /skills/guide\n\n\
             Read this before answering about bingo itself.",
        )
    }

    /// The user-directed rule of 2026-09-02: a skill is one row however it was
    /// asked for. What the person typed stays in the item — a rewind reads it
    /// back off there — but the row is the run, and the page it expanded to
    /// hangs under it as any result does.
    #[test]
    fn a_typed_skill_reads_as_the_run_it_is() {
        assert_eq!(
            drawn_knowing_the_skill(vec![skill_prompt()]),
            vec![
                "❖ Skill(guide) the wire format".to_string(),
                "  ⎿  Base directory for this skill: /skills/guide".to_string(),
                String::new(),
                "     Read this before answering about bingo itself.".to_string(),
            ],
        );
    }

    /// The catalogue is what says a name is a skill. A command it files
    /// elsewhere — and every command, before the catalogue has landed — keeps
    /// the row it always had: the line somebody typed.
    #[test]
    fn a_command_the_catalogue_calls_no_skill_is_the_line_that_was_typed() {
        assert_eq!(
            drawn(vec![skill_prompt()]),
            vec![
                "⏺ /guide the wire format".to_string(),
                "  ⎿  Base directory for this skill: /skills/guide".to_string(),
                String::new(),
                "     Read this before answering about bingo itself.".to_string(),
            ],
        );
    }

    /// The other door, on the same row: the model's own call names the skill
    /// in `input.name` and hands it `input.arguments`. The row is not written
    /// out again here — it is asserted to be the one above, which is the whole
    /// of what "one skill row" means.
    #[test]
    fn the_models_own_call_draws_the_same_row() {
        let called = drawn(vec![tool(
            "itm_1",
            "Skill",
            serde_json::json!({"name": "guide", "arguments": "the wire format"}),
            Some(ToolOutput::text(
                "Base directory for this skill: /skills/guide",
            )),
            ItemStatus::Completed,
        )]);
        assert_eq!(
            called.first(),
            drawn_knowing_the_skill(vec![skill_prompt()]).first(),
            "one run, one row, whichever door it came through",
        );
        assert_eq!(
            called,
            vec![
                "❖ Skill(guide) the wire format".to_string(),
                "  ⎿  Base directory for this skill: /skills/guide".to_string(),
            ],
            "the catalogue is not consulted: the call says what it is",
        );
    }

    /// A skill asked for with nothing to substitute is the signature alone.
    #[test]
    fn a_skill_with_no_arguments_is_the_signature_alone() {
        assert_eq!(
            drawn(vec![tool(
                "itm_1",
                "Skill",
                serde_json::json!({"name": "guide"}),
                None,
                ItemStatus::Running,
            )]),
            vec!["❖ Skill(guide)".to_string()],
        );
    }

    /// A call that names no skill is the tool it is: the fallback never
    /// invents a name for the screen.
    #[test]
    fn a_skill_call_that_names_nothing_is_drawn_as_a_tool_row() {
        assert_eq!(
            drawn(vec![tool(
                "itm_1",
                "Skill",
                serde_json::json!({"arguments": "the wire format"}),
                None,
                ItemStatus::Running,
            )]),
            vec!["⏺ Skill(the wire format)".to_string()],
        );
    }

    /// A message from another session says who sent it, as a room's post does.
    #[test]
    fn an_agents_message_names_the_agent_on_the_marked_line() {
        assert_eq!(
            drawn(vec![delivered(
                "itm_1",
                "agent",
                Some("reviewer"),
                "done, two nits"
            )]),
            vec!["⏺ reviewer: done, two nits".to_string()],
        );
    }

    /// A long notice folds the way a long result does — and offers no key,
    /// because `ctrl+o` reaches a result and this is not one.
    #[test]
    fn a_long_notice_folds_without_promising_a_key() {
        let body: String = (1..=9).map(|i| format!("\nline {i}")).collect();
        let drawn = drawn(vec![delivered(
            "itm_1",
            "schedule",
            None,
            &format!("the nightly run is in{body}"),
        )]);
        assert_eq!(drawn.len(), OUTPUT_ROWS + 2);
        assert_eq!(drawn[0], "⏺ the nightly run is in");
        assert_eq!(drawn.last().map(String::as_str), Some("     … +4 lines"));
    }

    #[test]
    fn the_model_speaks_after_a_bold_bullet_and_keeps_its_indent() {
        assert_eq!(
            drawn(vec![assistant(
                "itm_1",
                "alpha bravo charlie delta echo foxtrot golf hotel india juliet",
                ItemStatus::Completed,
            )]),
            vec![
                "⏺ alpha bravo charlie delta echo foxtrot golf hotel india".to_string(),
                "  juliet".to_string(),
            ],
            "a wrapped answer hangs under its own bullet"
        );
    }

    #[test]
    fn a_tool_row_names_the_call_and_hangs_its_result_under_it() {
        let output = ToolOutput::text("Read 3 lines");
        assert_eq!(
            drawn(vec![tool(
                "itm_1",
                "Read",
                serde_json::json!({"file_path": "/tmp/project/Cargo.toml"}),
                Some(output),
                ItemStatus::Completed,
            )]),
            vec![
                "⏺ Read(Cargo.toml)".to_string(),
                "  ⎿  Read 3 lines".to_string(),
            ],
            "the path is named from the session's own directory"
        );
    }

    #[test]
    fn a_long_result_says_how_much_it_folded_away_and_what_opens_it() {
        let output = ToolOutput::text((1..=9).map(|i| format!("line {i}\n")).collect::<String>());
        let drawn = drawn(vec![tool(
            "itm_1",
            "Read",
            serde_json::json!({"file_path": "src/lib.rs"}),
            Some(output),
            ItemStatus::Completed,
        )]);
        assert_eq!(drawn.len(), OUTPUT_ROWS + 2);
        assert_eq!(
            drawn.last().map(String::as_str),
            Some("     … +4 lines (ctrl+o to expand)")
        );
    }

    #[test]
    fn a_running_tool_shows_the_last_rows_of_its_tail() {
        let state = folded(vec![started_tool(
            1,
            running_tool("itm_1", "Bash", "one\ntwo\nthree\nfour"),
        )]);
        assert_eq!(
            rendered(&state),
            vec![
                "⏺ Bash(cargo test)".to_string(),
                "  ⎿  two".to_string(),
                "     three".to_string(),
                "     four".to_string(),
            ],
        );
    }

    /// A thought, once it is one: how long it took, and what it was.
    fn thought_item(text: &str, seconds: i64) -> Item {
        let mut item = item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::Reasoning {
                text: text.into(),
                provider_metadata: Default::default(),
            },
        );
        item.completed_at = Some(ts() + jiff::SignedDuration::from_secs(seconds));
        item
    }

    /// A thought as it is being had: no `completed_at`, and as much text as
    /// the deltas have carried so far.
    fn thinking_item(text: &str) -> Item {
        item(
            "itm_1",
            ItemStatus::Running,
            ItemBody::Reasoning {
                text: text.into(),
                provider_metadata: Default::default(),
            },
        )
    }

    #[test]
    fn thinking_streams_and_then_closes_to_the_row_alone() {
        assert_eq!(
            drawn(vec![thinking_item("the manifest")]),
            vec!["✻ Thinking…".to_string(), "  ⎿  the manifest".to_string()],
            "a thought being had is read as it is thought"
        );
        assert_eq!(
            drawn(vec![thought_item("The manifest first.", 2)]),
            vec!["✻ Thought for 2s".to_string()],
            "and once it is over it closes: the text is a click away, not a row"
        );
    }

    /// Two rows, the newest of what has arrived — the same tail a running tool
    /// wears (§6) and two rows of it rather than three, because a thought is
    /// read past.
    #[test]
    fn a_thought_being_had_streams_two_rows_of_its_newest_text() {
        assert_eq!(
            drawn(vec![thinking_item("first\nsecond\nthird\nfourth")]),
            vec![
                "✻ Thinking…".to_string(),
                "  ⎿  third".to_string(),
                "     fourth".to_string(),
            ],
        );
    }

    /// The three states a thought that is over has, which is the whole of what
    /// a click on one walks (§7). It is the one block with a shut.
    #[test]
    fn a_finished_thought_is_shut_then_peeks_from_the_top_then_opens_whole() {
        let text: String = (1..=9).map(|i| format!("step {i}\n")).collect();
        let state = folded(vec![frame(
            1,
            Event::ItemCompleted {
                item: thought_item(&text, 3),
            },
        )]);
        let at = |fold| {
            let folds: Folds = [(ItemId::from_raw("itm_1"), fold)].into_iter().collect();
            rendered_with(&state, &folds, &[])
        };
        assert_eq!(at(Fold::Shut), vec!["✻ Thought for 3s".to_string()]);
        assert_eq!(
            at(Fold::Peek),
            vec![
                "✻ Thought for 3s".to_string(),
                "  ⎿  step 1".to_string(),
                "     step 2".to_string(),
                "     … +7 lines (ctrl+o to expand)".to_string(),
            ],
            "the peek reads from the top: nothing is moving to follow"
        );
        let open = at(Fold::Open);
        assert_eq!(open.len(), 10, "the row and every step: {open:?}");
        assert_eq!(open.last().map(String::as_str), Some("     step 9"));
    }

    /// Nothing thought yet, and nothing to fold: the row alone, as an empty
    /// thought stays once it is over.
    #[test]
    fn a_thought_with_nothing_in_it_yet_is_the_row_alone() {
        assert_eq!(
            drawn(vec![thinking_item("")]),
            vec!["✻ Thinking…".to_string()],
        );
    }

    /// ADR-0035 §4: an ACP agent runs its own tools and journals each finished
    /// call as a reasoning item whose metadata is the whole call. It draws as
    /// the call it was — the same bullet, signature and folded result every
    /// other call wears — and the heading the item's text carries is what the
    /// row replaces rather than repeats.
    #[test]
    fn an_agents_own_call_draws_as_a_tool_row_and_not_as_a_thought() {
        assert_eq!(
            drawn(vec![agent_call(
                "itm_1",
                "read Read src/lib.rs (1 - 50)done\npub mod wire;",
                serde_json::json!({
                    "external": true,
                    "toolCallId": "toolu_01Read",
                    "title": "Read src/lib.rs (1 - 50)",
                    "kind": "read",
                    "status": "completed",
                    "content": [
                        { "type": "content",
                          "content": { "type": "text", "text": "pub mod wire;" } }
                    ],
                    "rawInput": { "file_path": "/tmp/project/src/lib.rs" },
                    "rawOutput": { "lines": 1 }
                }),
            )]),
            vec![
                "⏺ Read(src/lib.rs)".to_string(),
                "  ⎿  pub mod wire;".to_string(),
            ],
        );
    }

    /// A call still being made carries no metadata — the provider writes it
    /// when the block closes — so it reads exactly as it did before the row
    /// existed: the heading, under `✻ Thinking…`.
    #[test]
    fn an_agents_call_still_running_reads_as_the_text_it_is() {
        assert_eq!(
            drawn(vec![thinking_item("read Read src/lib.rs (1 - 50)")]),
            vec![
                "✻ Thinking…".to_string(),
                "  ⎿  read Read src/lib.rs (1 - 50)".to_string(),
            ],
        );
    }

    /// And a thought is a thought: no metadata, no row. The mark is the whole
    /// of what tells the two apart.
    #[test]
    fn a_thought_without_the_mark_is_untouched() {
        assert_eq!(
            drawn(vec![thought_item("The manifest first.", 2)]),
            vec!["✻ Thought for 2s".to_string()],
        );
        assert_eq!(
            drawn(vec![agent_call(
                "itm_1",
                "The manifest first.",
                serde_json::json!({ "title": "not a call" }),
            )]),
            vec!["✻ Thought for 1s".to_string()],
            "a namespace without the flag is somebody's private note, not a call"
        );
    }

    /// A call that failed says so where every other row says it: the bullet.
    /// Nothing about the row is invented for it — the status is the agent's.
    #[test]
    fn a_failed_call_wears_the_row_a_failed_tool_wears() {
        let state = stated(vec![agent_call(
            "itm_1",
            "tool toolu_04Bashfailed\nno such file",
            serde_json::json!({
                "external": true,
                "toolCallId": "toolu_04Bash",
                "title": "toolu_04Bash",
                "status": "failed",
                "content": [
                    { "type": "content",
                      "content": { "type": "text", "text": "no such file" } }
                ]
            }),
        )]);
        assert_eq!(
            rendered(&state),
            vec![
                "⏺ Tool(toolu_04Bash)".to_string(),
                "  ⎿  no such file".to_string(),
            ],
        );
        let item = state.items.first().expect("the call");
        assert_eq!(fold::fold_of(&Folds::new(), item), Fold::Peek);
    }

    /// Under a second is a moment, not no time at all.
    #[test]
    fn a_thought_shorter_than_a_second_says_so() {
        assert_eq!(took(jiff::SignedDuration::from_millis(400)), "<1s");
        assert_eq!(took(jiff::SignedDuration::from_secs(1)), "1s");
        assert_eq!(took(jiff::SignedDuration::from_secs(-1)), "<1s");
    }

    /// However long a thought was, the row a person meets is the same one row
    /// — a five-row cut of somebody else's working is what it stopped being.
    #[test]
    fn a_long_thought_costs_the_transcript_one_row() {
        let text: String = (1..=99).map(|i| format!("step {i}\n")).collect();
        assert_eq!(
            drawn(vec![thought_item(&text, 3)]),
            vec!["✻ Thought for 3s".to_string()],
        );
    }

    /// Redacted thinking, or a turn the provider summarised nothing of: the
    /// row alone, with no fold under it and no key promised.
    #[test]
    fn an_empty_thought_is_the_row_and_nothing_else() {
        assert_eq!(
            drawn(vec![thought_item("", 1)]),
            vec!["✻ Thought for 1s".to_string()],
        );
        assert_eq!(thought(&thought_item("   \n", 1)), None);
        assert_eq!(thought(&thought_item("why", 1)), Some("why"));
    }

    #[test]
    fn a_receipt_joins_the_row_that_asked_for_it() {
        assert_eq!(
            drawn(vec![
                tool(
                    "itm_1",
                    "Edit",
                    serde_json::json!({"file_path": "src/lib.rs"}),
                    None,
                    ItemStatus::Failed,
                ),
                receipt_item("itm_2", "Edit", DecisionKind::Deny, Some("use cargo clean")),
            ]),
            vec![
                "⏺ Edit(src/lib.rs)".to_string(),
                "  ⎿  denied — use cargo clean".to_string(),
            ],
            "no blank line, and the tool is named once"
        );
    }

    #[test]
    fn a_receipt_with_no_call_above_it_names_its_own_tool() {
        assert_eq!(
            drawn(vec![receipt_item(
                "itm_1",
                "Edit",
                DecisionKind::Allow,
                None
            )]),
            vec!["  ⎿  Edit allowed".to_string()],
        );
    }

    #[test]
    fn a_failed_turn_is_a_bullet_of_its_own() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                completed(
                    "trn_1",
                    TurnStatus::Failed {
                        error: bingo_sdk::KernelError::new(
                            bingo_sdk::ErrorCode::ProviderUnavailable,
                            "no route to the provider",
                        ),
                    },
                ),
            ),
        ]);
        assert_eq!(
            rendered(&state).last().map(String::as_str),
            Some("⏺ no route to the provider")
        );
    }

    #[test]
    fn the_measure_stops_prose_at_a_hundred_columns() {
        let state = folded(vec![frame(
            1,
            Event::ItemCompleted {
                item: assistant("itm_1", &"word ".repeat(60), ItemStatus::Completed),
            },
        )]);
        let welcomed = crate::welcome::lines(&state, 160).len();
        let mut blocks = crate::blocks::Blocks::default();
        let folds = Folds::new();
        let rows = Rows::of(&state, 160, &folds, &[], scene().1);
        let height = blocks.sync(&state, &Agents::new(), &rows, Vec::new());
        let widest = blocks
            .window(0, height)
            .iter()
            .skip(welcomed)
            .map(|line| line.to_string().trim_end().width())
            .max()
            .unwrap_or(0);
        assert!(widest <= wrap::MEASURE, "{widest} cells");
        assert!(widest > 80, "and it uses the measure it has: {widest}");
    }

    #[test]
    fn an_opened_result_shows_every_line() {
        let output = ToolOutput::text((1..=9).map(|i| format!("line {i}\n")).collect::<String>());
        let items = vec![tool(
            "itm_1",
            "Read",
            serde_json::json!({"file_path": "src/lib.rs"}),
            Some(output),
            ItemStatus::Completed,
        )];
        let frames = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| frame(i as u64 + 1, Event::ItemCompleted { item }))
            .collect();
        let state = folded(frames);
        let opened: Folds = [(ItemId::from_raw("itm_1"), Fold::Open)]
            .into_iter()
            .collect();
        let rows = rendered_with(&state, &opened, &[]).join("\n");
        assert!(rows.contains("line 9"), "{rows}");
        assert!(!rows.contains("+4 lines"), "{rows}");
    }

    #[test]
    fn a_tab_in_a_result_runs_to_the_next_stop_of_eight() {
        assert_eq!(expand_tabs("     1\t[package]"), "     1  [package]");
        assert_eq!(expand_tabs("a\tb\tc"), "a       b       c");
        assert_eq!(expand_tabs("no tabs"), "no tabs");
    }
}
