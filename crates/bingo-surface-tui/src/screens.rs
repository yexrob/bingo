//! Every screen of `docs/design/tui.md` §3, drawn at 80×24 and 120×40 and
//! read as a screen rather than as a diff. A snapshot here is the record of a
//! taste decision: if it stops reading the way §2 and §4 say, the change that
//! did it is the one to argue with.

use bingo_sdk::{
    ContentPart, ContextUsage, Event, InterruptReason, ItemBody, ItemStatus, KernelError, Level,
    LoginFlow, Preview, ToolOutput, TurnId, TurnStatus, View,
};
use serde_json::json;
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::painted::{ascii, assert_row_styled, daylight, in_look, no_colour, painted, truecolor};
use crate::test_support::*;
use crate::tree::Tree;
use crate::ui::{Open, Switcher, Ui};

/// Where the colour lands on these same scenes (§4).
mod colours;
/// What a list a cursor walks draws when it outgrows its room (§3).
mod windows;

/// One scene, at the two sizes every layout rule is written for.
fn both(name: &str, tree: &Tree, ui: &Ui, now: Now) {
    insta::assert_snapshot!(format!("{name}_80x24"), draw_tree(80, 24, tree, ui, now));
    insta::assert_snapshot!(format!("{name}_120x40"), draw_tree(120, 40, tree, ui, now));
}

fn item(seq: u64, item: bingo_sdk::Item) -> bingo_sdk::Frame {
    frame(seq, Event::ItemCompleted { item })
}

/// A session with something behind it: a question asked and answered.
fn answered() -> Vec<bingo_sdk::Frame> {
    vec![
        item(1, user("itm_1", "run the tests")),
        item(
            2,
            assistant(
                "itm_2",
                "All 33 pass.\n\n- `wrap` is done\n- `keys` is done",
                ItemStatus::Completed,
            ),
        ),
    ]
}

/// A card of `kind`, open and focused, as a person meets it.
fn asked(interaction: bingo_sdk::Interaction) -> (Tree, Ui, Now) {
    let state = folded(vec![frame(1, opened(interaction))]);
    let (mut ui, now) = settled();
    ui.dialog.focus_on(state.interactions.first());
    (solo(&state), ui, now)
}

#[test]
fn idle() {
    let (ui, now) = scene();
    both("idle", &solo(&folded(answered())), &ui, now);
}

#[test]
fn welcome_on_a_fresh_session() {
    let (ui, now) = scene();
    both("welcome", &solo(&state()), &ui, now);
}

#[test]
fn streaming() {
    let state = folded(vec![
        frame(1, started("trn_1")),
        item(2, user("itm_1", "explain the layout")),
        frame(
            3,
            Event::ItemStarted {
                item: assistant("itm_2", "", ItemStatus::Running),
            },
        ),
        frame(
            4,
            Event::ItemDelta {
                item: bingo_sdk::ItemId::from_raw("itm_2"),
                n: 0,
                kind: bingo_sdk::DeltaKind::Text,
                data:
                    "# The frame\n\nThe transcript starts at the top edge, and a half-written **sen"
                        .into(),
            },
        ),
    ]);
    let (ui, now) = mid_turn();
    both("streaming", &solo(&state), &ui, now);
}

#[test]
fn tool_running() {
    let state = folded(vec![
        frame(1, started("trn_1")),
        started_tool(
            2,
            running_tool(
                "itm_1",
                "Bash",
                "   Compiling bingo-sdk v0.1.0\n   Compiling bingo-core v0.1.0\n   Compiling bingo-surface-tui v0.1.0",
            ),
        ),
    ]);
    let (ui, now) = mid_turn();
    both("tool_running", &solo(&state), &ui, now);
}

/// Two commands the model asked for in one step. The executor runs a batch of
/// concurrency-safe allowed calls together, so neither row is waiting on the
/// other: both are live at the same moment.
fn running_together() -> Vec<bingo_sdk::Frame> {
    vec![
        frame(1, started("trn_1")),
        item(2, user("itm_0", "check the formatting and the tests")),
        started_tool(3, running_command("itm_1", "cargo fmt --all -- --check")),
        started_tool(4, running_command("itm_2", "cargo test --workspace")),
    ]
}

#[test]
fn two_tools_running_at_once() {
    let (ui, now) = mid_turn();
    both(
        "tools_running_together",
        &solo(&folded(running_together())),
        &ui,
        now,
    );
}

#[test]
fn tool_done_with_output() {
    let output = ToolOutput {
        parts: vec![ContentPart::text(
            (1..=9).map(|i| format!("line {i}\n")).collect::<String>(),
        )],
        is_error: false,
        display: None,
    };
    let state = folded(vec![
        item(1, user("itm_0", "read the manifest")),
        item(
            2,
            tool(
                "itm_1",
                "Read",
                json!({"file_path": "/tmp/project/crates/bingo-surface-tui/src/lib.rs"}),
                Some(output),
                ItemStatus::Completed,
            ),
        ),
        item(
            3,
            item_of(ItemBody::PermissionReceipt {
                interaction: bingo_sdk::InteractionId::from_raw("int_1"),
                tool: "Read".into(),
                decision: bingo_sdk::DecisionKind::AllowSession,
                feedback: None,
            }),
        ),
    ]);
    let (ui, now) = scene();
    both("tool_done", &solo(&state), &ui, now);
}

fn item_of(body: ItemBody) -> bingo_sdk::Item {
    crate::test_support::item("itm_2", ItemStatus::Completed, body)
}

#[test]
fn a_diff_result_sits_on_its_tints() {
    let state = folded(vec![item(
        1,
        tool(
            "itm_1",
            "Edit",
            json!({"file_path": "/tmp/project/src/lib.rs"}),
            Some(diff_output()),
            ItemStatus::Completed,
        ),
    )]);
    let (ui, now) = scene();
    both("diff_result", &solo(&state), &ui, now);
}

#[test]
fn permission_collapsed() {
    let (tree, ui, now) = asked(permission(Some("Edit(src/)"), Some(long_diff())));
    both("permission_collapsed", &tree, &ui, now);
}

#[test]
fn permission_expanded() {
    let (tree, mut ui, now) = asked(permission(Some("Edit(src/)"), Some(long_diff())));
    crate::input::on_key(&mut ui, &tree, ctrl('e'), now);
    both("permission_expanded", &tree, &ui, now);
}

#[test]
fn permission_with_the_feedback_row_open() {
    let mut asking = permission(
        None,
        Some(Preview::Command {
            command: "rm -rf build".into(),
            cwd: "/tmp/project".into(),
        }),
    );
    if let bingo_sdk::InteractionKind::Permission { summary, tool, .. } = &mut asking.kind {
        *tool = "Bash".into();
        *summary = "Bash rm -rf build".into();
    }
    let (tree, mut ui, now) = asked(asking);
    crate::input::on_key(&mut ui, &tree, typed('n'), now);
    write(&mut ui, tree.root(), "use cargo clean", now);
    both("permission_feedback", &tree, &ui, now);
}

/// A path far longer than the row it has to fit in.
#[test]
fn a_long_path_fits_one_row() {
    let deep = format!("/tmp/project/{}note.txt", "some-directory/".repeat(8));
    let mut asking = permission(Some("Write(src/)"), None);
    if let bingo_sdk::InteractionKind::Permission { summary, tool, .. } = &mut asking.kind {
        *tool = "Write".into();
        *summary = format!("Write {deep}");
    }
    let (tree, ui, now) = asked(asking);
    both("long_path", &tree, &ui, now);
}

#[test]
fn question_single() {
    let (tree, ui, now) = asked(question(false, false));
    both("question_single", &tree, &ui, now);
}

#[test]
fn question_multi() {
    let (tree, mut ui, now) = asked(question(true, true));
    crate::input::on_key(&mut ui, &tree, typed(' '), now);
    both("question_multi", &tree, &ui, now);
}

#[test]
fn confirm() {
    let (tree, ui, now) = asked(crate::test_support::confirm());
    both("confirm", &tree, &ui, now);
}

#[test]
fn login_browser() {
    let (tree, ui, now) = asked(login(LoginFlow::Browser {
        url: "https://auth.openai.com/oauth/authorize?client_id=app_x&state=s1".into(),
    }));
    both("login_browser", &tree, &ui, now);
}

#[test]
fn login_device() {
    let (tree, ui, now) = asked(login(LoginFlow::Device {
        url: "https://auth.openai.com/codex/device".into(),
        code: "ABCD-EFGH".into(),
    }));
    both("login_device", &tree, &ui, now);
}

#[test]
fn login_paste() {
    let (tree, mut ui, now) = asked(login(LoginFlow::Paste));
    crate::input::on_key(&mut ui, &tree, typed('1'), now);
    write(&mut ui, tree.root(), "sk-pasted-elsewhere", now);
    both("login_paste", &tree, &ui, now);
}

#[test]
fn an_error_turn() {
    let state = folded(vec![
        item(1, user("itm_0", "run the tests")),
        item(
            2,
            tool(
                "itm_1",
                "Bash",
                json!({"command": "cargo test"}),
                Some(ToolOutput::error("exit 101")),
                ItemStatus::Failed,
            ),
        ),
        frame(
            3,
            completed(
                "trn_1",
                TurnStatus::Failed {
                    error: KernelError::new(
                        bingo_sdk::ErrorCode::ProviderUnavailable,
                        "no route to the provider",
                    ),
                },
            ),
        ),
    ]);
    let (ui, now) = scene();
    both("error_turn", &solo(&state), &ui, now);
}

#[test]
fn an_interrupted_turn() {
    let state = folded(vec![
        item(1, user("itm_1", "long job")),
        item(
            2,
            crate::test_support::item(
                "itm_2",
                ItemStatus::Completed,
                ItemBody::Interruption {
                    marker: "[Request interrupted by the user]".into(),
                },
            ),
        ),
        frame(
            3,
            completed(
                "trn_1",
                TurnStatus::Interrupted {
                    reason: InterruptReason::UserCancel,
                },
            ),
        ),
    ]);
    let (ui, now) = scene();
    both("interrupted", &solo(&state), &ui, now);
}

/// A thought that has finished, with what it thought and how long it took.
fn thought(id: &str, text: &str, seconds: i64) -> bingo_sdk::Item {
    let mut thought = crate::test_support::item(
        id,
        ItemStatus::Completed,
        ItemBody::Reasoning {
            text: text.into(),
            provider_metadata: Default::default(),
        },
    );
    thought.completed_at = Some(ts() + jiff::SignedDuration::from_secs(seconds));
    thought
}

#[test]
fn thinking_and_its_decay() {
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", "The manifest first.", 2)),
        item(
            3,
            assistant("itm_3", "One package, demo 0.1.0.", ItemStatus::Completed),
        ),
    ]);
    let (ui, now) = scene();
    both("thinking", &solo(&state), &ui, now);
}

/// A thought is readable where it happened (M34): the row says how long it
/// took, what was thought hangs under it in dim, and the rest folds away
/// behind the key a result folds behind.
#[test]
fn a_long_thought_is_read_under_its_own_row() {
    let text = "The manifest first, because the lockfile only says what the manifest \
                already asked for.\n\
                Then the crate map, which is the one place the layering is written down.\n\
                The plan after that: it says which of the two is allowed to move.\n\
                Only then the code, and only the file the plan names.\n\
                Anything else is a second reading of the same three facts.\n\
                So: manifest, map, plan.";
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", text, 4)),
    ]);
    let (ui, now) = scene();
    both("reasoning_inline", &solo(&state), &ui, now);
}

/// Anthropic's redacted thinking, and an OpenAI turn the provider summarised
/// nothing of: the row alone. Nothing folds, so nothing is promised.
#[test]
fn an_empty_thought_draws_the_row_alone() {
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", "", 2)),
    ]);
    let (ui, now) = scene();
    both("reasoning_redacted", &solo(&state), &ui, now);
}

/// What a background job says when it ends, first line first.
const JOB: &str = "Background job ab12cd34 (`cargo test --workspace`) exited with code 0 after \
                   2m 4s.\n`BashOutput` with id \"ab12cd34\" reads what it wrote.";

/// The three sides of one rule in one transcript: a job reporting in and an
/// agent answering are the machinery, and a person writing from a channel is
/// a person.
fn reported() -> Vec<bingo_sdk::Frame> {
    vec![
        item(1, user("itm_1", "run the tests in the background")),
        item(
            2,
            assistant("itm_2", "Started. I will say.", ItemStatus::Completed),
        ),
        item(3, delivered("itm_3", "bash", None, JOB)),
        item(
            4,
            delivered("itm_4", "agent", Some("reviewer"), "Two nits, else fine."),
        ),
        item(
            5,
            delivered("itm_5", "channels", Some("mei"), "look at the deploy?"),
        ),
    ]
}

/// The machinery reporting in: each notice is a marked line with what to do
/// about it under it, not a band the width of the screen. A person's own
/// words get those, and so does a correspondent the closed set does not name.
#[test]
fn quiet_notices() {
    let (ui, now) = scene();
    both("quiet_notices", &solo(&folded(reported())), &ui, now);
}

/// A skill's prompt as the kernel journals it: the line the person typed,
/// then what the command made of it.
const SKILL_PROMPT: &str = "/guide the wire format\n\n\
     Base directory for this skill: ~/.bingo/skills/guide\n\n\
     Read this before answering a question about bingo itself.\n\n\
     - the kernel is small, and everything else is a plugin\n\
     - one ordered event stream, and every surface is a client of it\n\
     - `--print` is the headless surface, `serve --stdio` the JSON-RPC one\n\
     - a skill is a directory under `.bingo/skills` with a `SKILL.md` in it\n\n\
     ARGUMENTS: the wire format";

/// `/guide` typed: the row is the line the person typed and the page the
/// command produced folds under it — the shape `Skill(guide)`, the model's
/// own way to the same body, already has.
#[test]
fn a_skill_command() {
    let state = folded(vec![
        item(1, delivered("itm_1", "command", None, SKILL_PROMPT)),
        item(
            2,
            assistant(
                "itm_2",
                "One `Event` enum, one ordered stream per session.",
                ItemStatus::Completed,
            ),
        ),
    ]);
    let (ui, now) = scene();
    both("skill_command", &solo(&state), &ui, now);
}

#[test]
fn a_room_transcript() {
    let tree = room_tree(vec![
        posted(2, "itm_1", "reviewer", "the plan is thin on tests"),
        posted(3, "itm_2", "scout", "M9's exit criteria cover them"),
        log_frame(
            4,
            Event::ItemCompleted {
                item: user("itm_3", "then let us ship it"),
            },
        ),
    ]);
    let (ui, now) = scene();
    both("room_transcript", &tree, &ui, now);
}

#[test]
fn a_child_row_while_it_runs() {
    let tree = spawned_tree(busy_child("reviewer"));
    let (ui, now) = scene();
    both("child_running", &tree, &ui, now);
}

#[test]
fn a_child_row_when_it_is_done() {
    let mut frames = busy_child("reviewer");
    frames.push(child_frame(6, completed("trn_9", TurnStatus::Completed)));
    let tree = spawned_tree(frames);
    let (ui, now) = scene();
    both("child_done", &tree, &ui, now);
}

#[test]
fn a_child_row_that_wants_a_person() {
    let mut frames = busy_child("reviewer");
    frames.push(child_frame(6, opened(child_permission())));
    let tree = spawned_tree(frames);
    let (mut ui, now) = settled();
    ui.dialog
        .focus_on(tree.open_interaction().map(|(_, open)| open));
    both("child_needs_you", &tree, &ui, now);
}

#[test]
fn a_child_transcript() {
    let mut frames = busy_child("reviewer");
    frames.push(child_frame(
        6,
        Event::ItemCompleted {
            item: user("itm_9", "review the diff"),
        },
    ));
    frames.push(child_frame(
        7,
        Event::ItemCompleted {
            item: assistant("itm_10", "Two nits, otherwise fine.", ItemStatus::Completed),
        },
    ));
    let mut tree = spawned_tree(frames);
    tree.show(&child_id());
    let (ui, now) = mid_turn();
    both("child_transcript", &tree, &ui, now);
}

#[test]
fn the_switcher_dropdown() {
    let mut frames = busy_child("reviewer");
    frames.push(log_frame(9, log_announced("#design")));
    let tree = spawned_tree(frames);
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Switcher(Switcher {
            selected: 2,
            ..Default::default()
        }),
        now,
    );
    both("switcher", &tree, &ui, now);
}

/// What was spawned in an earlier process is in the store and not here
/// (M31): its row sits under the live ones, dim, and says so in a word.
#[test]
fn the_switcher_lists_what_is_only_in_the_store() {
    let tree = spawned_tree(busy_child("reviewer"));
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Switcher(Switcher {
            selected: 2,
            stored: vec![stored_summary("ses_7", "scout")],
        }),
        now,
    );
    both("switcher_stored", &tree, &ui, now);
}

/// The quick cycle: `↓` on an empty composer, so the strip has the status
/// line's row and a chip of each kind is on it (§3, 2026-08-31).
#[test]
fn the_quick_cycle_strip() {
    let mut frames = busy_child("reviewer");
    frames.push(log_frame(9, log_announced("#design")));
    let tree = spawned_tree(frames);
    let (mut ui, now) = scene();
    ui.cycling = true;
    both("quick_cycle", &tree, &ui, now);

    // §4: the row spends a hue on the cursor and on the one session at work,
    // and on nothing else — which chip is on screen is said in weight.
    let painted = painted(80, 24, &tree, &ui, now);
    assert_eq!(painted.coloured("#design"), vec!["❯", "⏺"]);
}

#[test]
fn the_command_dropdown() {
    let state = folded(answered());
    let (mut ui, now) = scene();
    ui.catalogs.commands = vec![bingo_sdk::CommandSpec {
        name: "model".into(),
        aliases: vec![],
        hint: "[provider/]model".into(),
        args: bingo_sdk::ArgSpec::Catalog {
            source: "models".into(),
        },
        instant: true,
        family: "kernel".into(),
    }];
    write(&mut ui, &state, "/", now);
    both("dropdown", &solo(&state), &ui, now);
}

#[test]
fn the_panel_sheet() {
    let tree = room_tree(vec![
        posted(2, "itm_1", "reviewer", "what is left?"),
        log_frame(
            3,
            extended(
                "bingo.tasks",
                "tasks",
                json!([
                    {"id": 1, "status": "pending", "subject": "write the plan"},
                    {"id": 2, "status": "in_progress", "subject": "ship it", "owner": "reviewer"},
                ]),
            ),
        ),
    ]);
    let (mut ui, now) = scene();
    shown(&mut ui, Open::Panel, now);
    both("panel_sheet", &tree, &ui, now);
}

#[test]
fn the_help_sheet() {
    let state = folded(answered());
    let (mut ui, now) = scene();
    shown(&mut ui, Open::Help, now);
    insta::assert_snapshot!("help_80x30", draw_sized(80, 30, &state, &ui, now));
    insta::assert_snapshot!("help_100x30", draw_sized(100, 30, &state, &ui, now));
}

#[test]
fn a_notice_and_a_context_warning() {
    let state = folded(vec![
        item(1, user("itm_1", "compact it")),
        frame(
            2,
            Event::TurnUsage {
                turn: TurnId::from_raw("trn_1"),
                usage: Default::default(),
                context: ContextUsage {
                    used: 185_000,
                    window: 200_000,
                    trigger: 180_000,
                },
            },
        ),
    ]);
    let (mut ui, now) = scene();
    ui.notify(Level::Error, "unknown command: /x", now.instant);
    both("notice", &solo(&state), &ui, now);
}

#[test]
fn a_view_a_command_answered() {
    let state = folded(answered());
    let (mut ui, now) = scene();
    ui.block = Some(View::Table {
        headers: vec!["mode".into(), "meaning".into()],
        rows: vec![
            vec!["default".into(), "ask for what is not allowed".into()],
            vec![
                "acceptEdits".into(),
                "edits inside the cwd are allowed".into(),
            ],
        ],
    });
    both("view_table", &solo(&state), &ui, now);
}

// ---- the looks a person can ask for -------------------------------------

#[test]
fn without_colour() {
    let (ui, now) = settled();
    let tree = solo(&folded(answered()));
    insta::assert_snapshot!(
        "no_colour_idle",
        in_look(no_colour(), || draw_tree(80, 24, &tree, &ui, now))
    );
    let (card, ui, now) = asked(permission(Some("Edit(src/)"), Some(long_diff())));
    insta::assert_snapshot!(
        "no_colour_permission",
        in_look(no_colour(), || draw_tree(80, 24, &card, &ui, now))
    );
    crate::theme::with(no_colour(), || {
        let painted = painted(80, 24, &card, &ui, now);
        for row in ["Do you want to", "1. Yes", "+line 1", "Edit"] {
            assert!(
                painted.coloured(row).is_empty(),
                "NO_COLOR spends none on {row:?}"
            );
        }
    });
}

/// The same frame over a light ground: nothing of the layout moves, and every
/// token is the other end of its own hue (design §4).
#[test]
fn in_daylight() {
    let (ui, now) = scene();
    let tree = solo(&folded(answered()));
    insta::assert_snapshot!(
        "light_idle",
        in_look(daylight(), || draw_tree(80, 24, &tree, &ui, now))
    );
    assert_eq!(
        in_look(daylight(), || draw_tree(80, 24, &tree, &ui, now)),
        in_look(truecolor(), || draw_tree(80, 24, &tree, &ui, now)),
        "a palette changes what a cell is worth, never which cell it is"
    );
    crate::theme::with(daylight(), || {
        let painted = painted(80, 24, &tree, &ui, now);
        let ground = crate::theme::as_drawn(crate::theme::raised()).bg;
        assert!(
            painted
                .row("run the tests")
                .iter()
                .all(|(_, style)| style.bg == ground),
            "what you said is still a band, on the light tint"
        );
    });
}

#[test]
fn without_the_glyphs() {
    let (ui, now) = scene();
    let tree = solo(&folded(answered()));
    insta::assert_snapshot!(
        "ascii_idle",
        in_look(ascii(), || draw_tree(80, 24, &tree, &ui, now))
    );
    let (card, ui, now) = asked(permission(Some("Edit(src/)"), Some(long_diff())));
    insta::assert_snapshot!(
        "ascii_permission",
        in_look(ascii(), || draw_tree(80, 24, &card, &ui, now))
    );
}

// ---- M11e: the content kinds of §5 --------------------------------------

/// An answer whose middle is a GFM table: ruled, its numbers down the right.
#[test]
fn a_markdown_table_is_ruled() {
    let state = folded(vec![
        item(1, user("itm_1", "how many tests are there?")),
        item(
            2,
            assistant(
                "itm_2",
                "Per crate:\n\n\
                 | crate | tests | time |\n\
                 |---|---|---|\n\
                 | sdk | 41 | 0.02 |\n\
                 | core | 137 | 1.40 |\n\
                 | surface-tui | 9 | |\n",
                ItemStatus::Completed,
            ),
        ),
    ]);
    let (ui, now) = scene();
    both("markdown_table", &solo(&state), &ui, now);
}

const RUST: &str = "// the frame, once\npub fn draw(now: Now) -> bool {\n    let ready = true;\n    ready && now.motion\n}\n";

/// A fenced block in an answer: the fence's word, the gutter, and the code
/// in the three inks of §5.
#[test]
fn a_highlighted_code_block() {
    let state = folded(vec![
        item(1, user("itm_1", "show me the draw function")),
        item(
            2,
            assistant(
                "itm_2",
                &format!("Here it is:\n\n```rust\n{RUST}```\n"),
                ItemStatus::Completed,
            ),
        ),
    ]);
    let (ui, now) = scene();
    both("code_block", &solo(&state), &ui, now);
}

/// `esc esc`: the turns of this transcript, newest first, above the input box
/// like the switcher (design §3 — the rewind picker is a card).
#[test]
fn the_rewind_picker() {
    let mut state = folded(answered());
    for item in state.items.iter_mut() {
        item.turn = Some(TurnId::from_raw("trn_1"));
    }
    state.items.push({
        let mut asked = user("itm_3", "now write me a note and run the tests");
        asked.turn = Some(TurnId::from_raw("trn_2"));
        asked
    });
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Rewind(crate::rewind::Rewind { selected: 1 }),
        now,
    );
    both("rewind_picker", &solo(&state), &ui, now);
}

/// `@` in the composer: the paths under the session's own directory, on the
/// same dropdown the `/` menu rides (design §4).
#[test]
fn the_at_completion_dropdown() {
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::create_dir_all(dir.path().join("src")).expect("a source directory");
    for name in ["lib.rs", "markdown.rs", "pager.rs", "theme.rs"] {
        std::fs::write(dir.path().join("src").join(name), "//! it\n").expect("a source");
    }
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("a manifest");
    // A transcript long enough that the welcome box — which would carry this
    // run's temporary directory into the snapshot — has scrolled away.
    let mut state = long_transcript(24);
    state.summary.cwd = dir.path().to_string_lossy().into_owned();
    let (mut ui, now) = scene();
    write(&mut ui, &state, "read @src/", now);
    both("at_dropdown", &solo(&state), &ui, now);
}

/// A long result, open whole: what `⏎` on a focused block and the second
/// `ctrl+o` both take (design §5).
#[test]
fn the_pager_sheet() {
    let output = ToolOutput::text(
        (1..=40)
            .map(|i| format!("crates/bingo-surface-tui/src/file_{i}.rs\n"))
            .collect::<String>(),
    );
    let state = folded(vec![
        item(1, user("itm_0", "list the sources")),
        item(
            2,
            tool(
                "itm_1",
                "Glob",
                json!({"pattern": "crates/**/*.rs"}),
                Some(output),
                ItemStatus::Completed,
            ),
        ),
    ]);
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Pager(crate::pager::Pager::open(bingo_sdk::ItemId::from_raw(
            "itm_1",
        ))),
        now,
    );
    both("pager_sheet", &solo(&state), &ui, now);
}

/// `⏎` on a `✻ Thought for 2s` row: what was thought, whole.
#[test]
fn a_thought_opens_in_a_sheet() {
    let state = folded(vec![item(
        1,
        thought(
            "itm_1",
            "The manifest first, because the lockfile only says what the\n\
             manifest already asked for.\n\n\
             Then the crate map, which is the one place the layering is\n\
             written down.",
            2,
        ),
    )]);
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Pager(crate::pager::Pager::open(bingo_sdk::ItemId::from_raw(
            "itm_1",
        ))),
        now,
    );
    both("reasoning_sheet", &solo(&state), &ui, now);
}

/// The card of §4 with a replacement under its title: the colour comes from
/// the column, the weight from the words that actually moved.
#[test]
fn a_permission_card_previews_a_word_level_diff() {
    let (tree, ui, now) = asked(permission(
        Some("Edit(src/)"),
        Some(Preview::Diff {
            unified: "--- a/src/view.rs\n+++ b/src/view.rs\n@@ -12,3 +12,3 @@\n fn border(busy: bool) -> Style {\n-    match busy { true => dim() }\n+    match busy { true => breathing() }\n }\n".into(),
        }),
    ));
    both("permission_word_diff", &tree, &ui, now);
    let painted = painted(80, 24, &tree, &ui, now);
    let emphasised: Vec<String> = painted
        .row("+    match busy")
        .into_iter()
        .filter(|(_, style)| style.add_modifier.contains(ratatui::style::Modifier::BOLD))
        .map(|(text, _)| text.trim().to_string())
        .collect();
    assert_eq!(emphasised, vec!["breathing()".to_string()]);
}

/// What a screen cannot show: which token every run of a fence was drawn in.
/// `keyword` is the one cool colour, `dim` a comment, `text` the rest — and a
/// fence that names a diff wears the diff's own tints instead.
#[test]
fn every_fenced_language_is_inked_by_the_token_table() {
    insta::assert_snapshot!("inked_rust", inked("rust", RUST));
    insta::assert_snapshot!(
        "inked_python",
        inked(
            "python",
            "# read it first\ndef run(path):\n    return open(path).read()\n"
        )
    );
    insta::assert_snapshot!(
        "inked_json",
        inked(
            "json",
            "{\n  \"model\": \"gpt-5.4\",\n  \"stream\": true\n}\n"
        )
    );
    insta::assert_snapshot!(
        "inked_diff",
        inked("diff", "@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n ok\n")
    );
}

/// One fence, as runs of text with the token each was spent on.
fn inked(lang: &str, code: &str) -> String {
    crate::markdown::render(&format!("```{lang}\n{code}```"), 60)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| format!("{}⟨{}⟩", span.content, token(span.style)))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn token(style: ratatui::style::Style) -> &'static str {
    use crate::theme;
    for (name, spent) in [
        ("keyword", theme::mode()),
        ("dim", theme::dim()),
        ("added·moved", theme::added().patch(theme::bold())),
        ("removed·moved", theme::removed().patch(theme::bold())),
        ("added", theme::added()),
        ("removed", theme::removed()),
        ("text", theme::text()),
    ] {
        if style == spent {
            return name;
        }
    }
    "?"
}
