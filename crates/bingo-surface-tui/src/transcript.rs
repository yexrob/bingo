//! One item to styled lines, in Claude Code's grammar (`docs/design/tui.md`
//! §4): `⏺` for what the model says and does, `⎿` for what came back, `>` on a
//! raised bar for what you said. The reducer is the only history: nothing here
//! remembers a thing, and [`crate::blocks`] stacks and memoises what it draws.

use std::time::{Duration, Instant};

use bingo_sdk::{
    CommandSpec, ContentPart, DecisionKind, Driver, Item, ItemBody, ItemStatus, SessionState,
    ToolOutput, TurnStatus, View,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::clock::{self, Anim, Now};
use crate::fold::{self, Fold, Folds};
use crate::graphics::{Decoded, Picture};
use crate::skill::{self, Run};
use crate::tree::{self, Agents};
use crate::{acp, markdown, paths, theme, views, wrap};

/// What was said into a session: a person's line, a subsystem's notice, a
/// room's conversation.
mod said;

/// The picture under the words, where the terminal draws one (§5).
mod pictured;

pub(crate) use said::quiet;

/// How long the comet tail of a block still arriving takes to cool (§6).
pub const COMET: Duration = Duration::from_millis(180);
/// How long one light takes to cross the name of a call that has just come
/// back: six frames (§6). A thirty-three millisecond flash is below the
/// threshold at which a person reads it as something happening.
pub const SWEEP: Duration = Duration::from_millis(6 * crate::clock::FRAME.as_millis() as u64);
/// How long a call that came back wrong takes to cool out of `bad` into the
/// words behind it: twelve frames (§6). A flare that settles, never a shake —
/// §3's "nothing jumps" outranks it.
pub const FLARE: Duration = Duration::from_millis(12 * crate::clock::FRAME.as_millis() as u64);
/// How long a block that has just landed goes on being drawn again: the
/// longer of the two cues one can wear, so neither is cut short.
pub const LANDING: Duration = match SWEEP.as_millis() > FLARE.as_millis() {
    true => SWEEP,
    false => FLARE,
};
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
    /// The pictures already turned into pixels. A row that draws one has to
    /// know how many pixels it is before it can say how many cells it takes,
    /// and that answer is kept rather than worked out again every frame.
    pub pictures: &'a Decoded,
    /// What the session in view is called. A post says which room it came
    /// from, and the room's own transcript is the one place that says nothing.
    pub title: Option<&'a str>,
    /// What answers the session in view. A room answers nobody (ADR-0011 §1),
    /// and that one fact is what tells a room's own transcript — where its
    /// conversation is read — from a member's, where none of it belongs
    /// (ADR-0034).
    pub driver: Driver,
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
        pictures: &'a Decoded,
        now: Now,
    ) -> Self {
        Self {
            cwd: &state.summary.cwd,
            width,
            folds,
            commands,
            now,
            pictures,
            title: state.summary.title.as_deref(),
            driver: state.summary.driver,
        }
    }
}

/// Where one block is in its own motion (§6): the clock [`crate::blocks`]
/// measured for it, and whether it flipped into being finished — which is
/// what the light across its name, and the cooling of one that failed, are
/// both measured from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cue {
    pub since: Instant,
    pub flip: bool,
}

/// What the words of a row that has just landed are doing: one light crossing
/// the name of a call that came back, the whole row cooling out of `bad` for
/// one that came back wrong, or nothing at all — which is what every frame
/// but the twelve after it draws.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Landing {
    Settled,
    Landed(f32),
    Failed(f32),
}

impl Landing {
    /// How far into its own motion this block is. A block that did not just
    /// finish, and a surface where nothing may move, are both settled.
    fn of(cue: Cue, failed: bool, rows: &Rows<'_>) -> Self {
        if !cue.flip || !rows.now.motion {
            return Landing::Settled;
        }
        let len = match failed {
            true => FLARE,
            false => SWEEP,
        };
        let come = Anim::new(cue.since, len).progress(rows.now.instant);
        match failed {
            true => Landing::Failed(come),
            false => Landing::Landed(come),
        }
    }

    /// What the words after the name are drawn in.
    fn about(self) -> Style {
        match self {
            Landing::Failed(come) => theme::cooling(come),
            _ => theme::text(),
        }
    }
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

/// One item's block: the lines it draws, and the pictures those lines stand
/// for. The two are one rendering — a placeholder cell says nothing without
/// the picture behind it — so they are answered together and kept together
/// ([`crate::blocks`]), and a frame that draws no picture carries none.
#[derive(Debug, Default)]
pub struct Block {
    pub lines: Vec<Line<'static>>,
    pub pictures: Vec<Picture>,
}

/// One item's block. `previous` is the item before it, which a receipt joins;
/// `agents` the sub-sessions this transcript's calls spawned.
pub fn item_block(
    item: &Item,
    previous: Option<&Item>,
    agents: &Agents<'_>,
    rows: &Rows<'_>,
    cue: Cue,
) -> Block {
    // How much of this block is on the screen — what `ctrl+o` and a click both
    // write, over the start its kind has. One question, asked once, here.
    let fold = fold::fold_of(rows.folds, item);
    pictured::under_the_words(
        item,
        item_lines(item, previous, agents, rows, cue, fold),
        fold,
        rows,
    )
}

/// The words of one item's block, which is every row of it but the picture.
fn item_lines(
    item: &Item,
    previous: Option<&Item>,
    agents: &Agents<'_>,
    rows: &Rows<'_>,
    cue: Cue,
    fold: Fold,
) -> Vec<Line<'static>> {
    match &item.body {
        ItemBody::User { parts, origin } => said::lines(item, parts, origin, fold, rows),
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
            action(item.status, name, args, result.as_ref(), fold, rows, cue)
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

impl Call<'_> {
    /// Whether it came back wrong: its own status, or an output the tool
    /// marked as an error. One question, asked here, so the bullet that says
    /// so and the words that cool out of it can never disagree.
    fn failed(&self) -> bool {
        self.status == ItemStatus::Failed || self.output.is_some_and(|output| output.is_error)
    }
}

/// The row one skill run wears, whichever door it came through: the model's
/// `Skill(guide)` call and a person's own `/guide` are the same thing
/// happening, so this is the only place either is drawn (design §4).
///
/// The mark is the skill's own glyph in the bullet's place, in the colour the
/// bullet would have worn: what kind of row it is and what state it is in are
/// two facts, and each keeps its own carrier.
fn skill_row(run: Run<'_>, style: Style, rows: &Rows<'_>, landing: Landing) -> Vec<Line<'static>> {
    marked(theme::skill(), style, vec![asked(run, rows, landing)], rows)
}

/// `Skill(guide) the wire format`: the call as any tool row spells one, then
/// the free text the skill was given — outside the parentheses, because it is
/// what the skill reads and not what it is.
fn asked(run: Run<'_>, rows: &Rows<'_>, landing: Landing) -> Line<'static> {
    let mut line = signature(skill::TOOL, run.name, rows, landing);
    if !run.args.is_empty() {
        line.spans
            .push(Span::styled(format!(" {}", run.args), landing.about()));
    }
    line
}

fn tool_call(call: Call<'_>, rows: &Rows<'_>, cue: Cue) -> Vec<Line<'static>> {
    let failed = call.failed();
    let style = live_bullet(call.status, failed, rows);
    let landing = Landing::of(cue, failed, rows);
    let mut out = match call.run {
        Some(run) => skill_row(run, style, rows, landing),
        None => speaks(
            style,
            vec![signature(call.name, &call.about, rows, landing)],
            rows,
        ),
    };
    out.extend(result(&call, rows));
    out
}

/// The bullet says what state the row is in; its motion says how fresh that
/// state is — it pulses between `presence` and its glow while the tool runs.
/// What the answer landing looks like is the row's own words ([`Landing`]),
/// not a frame of weight on the bullet: a thirty-three millisecond flash is
/// below the threshold at which a person reads it as something happening.
fn live_bullet(status: ItemStatus, failed: bool, rows: &Rows<'_>) -> Style {
    let settled = bullet_style(status, failed);
    match rows.now.motion && status == ItemStatus::Running {
        true => theme::pulse(clock::breath(rows.now, PULSE)),
        false => settled,
    }
}

/// `Read(Cargo.toml)`: the name bold, what it is about plain — and, for the
/// twelve frames after the call comes back, whatever its landing is doing to
/// the two of them.
fn signature(name: &str, about: &str, rows: &Rows<'_>, landing: Landing) -> Line<'static> {
    let mut spans = named(name, landing);
    let about = rows.shorten(about);
    if !about.is_empty() {
        spans.push(Span::styled(format!("({about})"), landing.about()));
    }
    Line::from(spans)
}

/// The name of the row: one span at rest, and one per cell while a light is
/// crossing it.
fn named(name: &str, landing: Landing) -> Vec<Span<'static>> {
    match landing {
        Landing::Settled => vec![Span::styled(name.to_string(), theme::bold())],
        Landing::Failed(come) => vec![Span::styled(
            name.to_string(),
            theme::cooling(come).patch(theme::bold()),
        )],
        Landing::Landed(come) => swept(name, come),
    }
}

/// The name with one light crossing it, cell by cell: the same sweep the box's
/// border runs when a line is sent, on the other side of the conversation.
fn swept(name: &str, come: f32) -> Vec<Span<'static>> {
    let cells = name.chars().count();
    name.chars()
        .enumerate()
        .map(|(at, glyph)| {
            Span::styled(
                glyph.to_string(),
                theme::landing(clock::sweep(come, at, cells)),
            )
        })
        .collect()
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
        vec![signature(
            &tree::name(child),
            &summarize(input),
            rows,
            Landing::Settled,
        )],
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
    cue: Cue,
) -> Vec<Line<'static>> {
    let failed = status == ItemStatus::Failed;
    let mut out = speaks(
        bullet_style(status, failed),
        vec![signature(
            name,
            &as_text(args),
            rows,
            Landing::of(cue, failed, rows),
        )],
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

    /// A member's own transcript: a session a model answers.
    fn drawn(items: Vec<Item>) -> Vec<String> {
        rendered(&stated(items))
    }

    /// The same items in the room's own transcript: a session nothing answers,
    /// under the room's name (ADR-0011 §1).
    fn drawn_in_room(items: Vec<Item>) -> Vec<String> {
        let mut state = stated(items);
        state.summary.title = Some("#design".to_string());
        state.summary.driver = Driver::Log;
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
        let pictures = Decoded::default();
        let rows = Rows::of(state, 60, folds, commands, &pictures, scene().1);
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

    /// The whole of ADR-0034 in one transcript: a room's activity is the
    /// room's, so a member's own — the holder's included — draws none of it,
    /// and what the person typed is still a block of their own.
    #[test]
    fn a_members_transcript_draws_none_of_its_rooms_machinery() {
        assert_eq!(
            drawn(vec![
                delivered(
                    "itm_1",
                    "room",
                    None,
                    "#design has posts you have not read."
                ),
                delivered(
                    "itm_2",
                    "contributor:rooms",
                    None,
                    "[#design, since you last read]\nscout: found it",
                ),
                post("itm_3", "reviewer", "two nits, otherwise fine"),
                person("itm_4", "thanks"),
            ]),
            vec!["> thanks".to_string()],
            "the nudge, the reading and a copied post are the room's"
        );
    }

    /// The set is exactly the two origins the rooms plugin writes: a message
    /// from a peer names a conversation of its own and still draws, and every
    /// other contributor still speaks.
    #[test]
    fn only_the_rooms_own_origins_are_kept_out_of_a_member() {
        assert_eq!(
            drawn(vec![delivered(
                "itm_1",
                "agent",
                Some("scout"),
                "Two nits, else fine."
            )]),
            vec!["⏺ scout: Two nits, else fine.".to_string()],
        );
        assert_eq!(
            drawn(vec![delivered(
                "itm_2",
                "contributor:experience:recall",
                None,
                "you have seen this before"
            )]),
            vec!["> you have seen this before".to_string()],
        );
    }

    /// The room's own transcript is where its activity is read, and every post
    /// it holds is drawn there — the name first, then the whole message.
    #[test]
    fn a_room_reads_as_a_conversation_does() {
        assert_eq!(
            drawn_in_room(vec![post(
                "itm_1",
                "scout",
                "found it\n\nit was the cursor all along",
            )]),
            vec![
                "⏺ scout: found it".to_string(),
                String::new(),
                "  it was the cursor all along".to_string(),
            ],
            "no `⎿`, nothing dim, nothing folded: a message is read whole"
        );
    }

    /// A post nobody signed came from the session the room hangs under, and it
    /// is read under the name every seat on the roster reads it under — the
    /// same row as anybody else's, because in the room it is anybody else's.
    #[test]
    fn an_unsigned_post_is_the_holders_and_says_so() {
        assert_eq!(
            drawn_in_room(vec![person("itm_1", "then let us ship it")]),
            vec!["⏺ parent: then let us ship it"],
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
    /// The room's own is quieter still — it is read in the room and nowhere
    /// else — so it is the one member of the set that draws no row here.
    #[test]
    fn every_quiet_surface_is_a_marked_line_and_nothing_else_is() {
        for surface in said::QUIET_SURFACES
            .iter()
            .filter(|surface| **surface != said::ROOMS)
        {
            assert_eq!(
                drawn(vec![delivered("itm_1", surface, None, "it ended")]),
                vec!["⏺ it ended".to_string()],
                "{surface} is quiet"
            );
        }
        assert!(
            drawn(vec![delivered("itm_1", said::ROOMS, None, "it ended")]).is_empty(),
            "and the room's own is not drawn in a member at all"
        );
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

    /// Every field of the mark but the flag is optional, so a journal an older
    /// build wrote still draws a row — the name it can state, and nothing else.
    #[test]
    fn a_thin_mark_from_an_older_build_still_draws_a_row() {
        assert_eq!(
            drawn(vec![agent_call(
                "itm_1",
                "tool something",
                serde_json::json!({ "external": true }),
            )]),
            vec!["⏺ Tool".to_string()],
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
        let pictures = Decoded::default();
        let rows = Rows::of(&state, 160, &folds, &[], &pictures, scene().1);
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
