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
use crate::painted::{ascii, assert_row_styled, in_look, no_colour, painted, truecolor};
use crate::test_support::*;
use crate::tree::Tree;
use crate::ui::{Open, Switcher, Ui};

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

#[test]
fn thinking_and_its_decay() {
    let mut thought = crate::test_support::item(
        "itm_2",
        ItemStatus::Completed,
        ItemBody::Reasoning {
            text: "The manifest first.".into(),
            provider_metadata: Default::default(),
        },
    );
    thought.completed_at = Some(ts() + jiff::SignedDuration::from_secs(2));
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought),
        item(
            3,
            assistant("itm_3", "One package, demo 0.1.0.", ItemStatus::Completed),
        ),
    ]);
    let (ui, now) = scene();
    both("thinking", &solo(&state), &ui, now);
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
    shown(&mut ui, Open::Switcher(Switcher { selected: 2 }), now);
    both("switcher", &tree, &ui, now);
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

// ---- where the colour lands (design §4) ---------------------------------

#[test]
fn an_answer_row_is_white_after_its_bullet() {
    let (ui, now) = scene();
    let tree = solo(&folded(answered()));
    let painted = painted(80, 24, &tree, &ui, now);
    assert_row_styled(
        &painted,
        "All 33 pass.",
        &[
            ("⏺ ", crate::theme::text().patch(crate::theme::bold())),
            ("All 33 pass.", crate::theme::text()),
        ],
    );
}

#[test]
fn a_tool_rows_bullet_is_the_only_cell_a_colour_is_spent_on() {
    let output = ToolOutput::text("Read 3 lines");
    let state = folded(vec![item(
        1,
        tool(
            "itm_1",
            "Read",
            json!({"file_path": "src/lib.rs"}),
            Some(output),
            ItemStatus::Completed,
        ),
    )]);
    let (ui, now) = scene();
    let painted = painted(80, 24, &solo(&state), &ui, now);
    assert_eq!(painted.coloured("Read(src/lib.rs)"), vec!["⏺".to_string()]);
    assert!(
        painted.coloured("Read 3 lines").is_empty(),
        "a result spends none at all"
    );
}

#[test]
fn a_card_spends_its_colour_on_the_row_the_keyboard_is_on() {
    let (tree, ui, now) = asked(permission(Some("Edit(src/)"), None));
    let painted = painted(80, 24, &tree, &ui, now);
    // The cursor, and the card's own border on either side of the row (§4:
    // a card has the only bright border on the screen).
    assert_eq!(
        painted.coloured("1. Yes"),
        vec!["│".to_string(), "❯".to_string(), "│".to_string()]
    );
    assert_eq!(
        painted.coloured("2. Yes, allow"),
        vec!["│".to_string(), "│".to_string()],
        "the rows it is not on carry the border and nothing else"
    );
    // Inside the border the question runs to the box's edge, in `text`.
    let question = format!(" {:<77}", "Do you want to edit src/lib.rs?");
    assert_row_styled(
        &painted,
        "Do you want to",
        &[
            ("│", crate::theme::presence()),
            (question.as_str(), crate::theme::text()),
            ("│", crate::theme::presence()),
        ],
    );
}

/// The one place `raised` is spent, and the only rule 24 bits can carry that
/// the eight cannot: what you said is a band, not a sentence.
#[test]
fn your_own_line_sits_on_a_bar_the_width_of_the_transcript() {
    let (ui, now) = scene();
    let tree = solo(&folded(answered()));
    crate::theme::with(truecolor(), || {
        let row = painted(80, 24, &tree, &ui, now).row("run the tests");
        let width: usize = row.iter().map(|(text, _)| text.width()).sum();
        assert_eq!(width, 80, "the bar runs to the edge: {row:#?}");
        let ground = crate::theme::as_drawn(crate::theme::raised()).bg;
        assert!(
            row.iter().all(|(_, style)| style.bg == ground),
            "every cell of it is on the raised ground: {row:#?}"
        );
    });
}

#[test]
fn the_status_line_spends_no_colour_but_the_mode() {
    let state = with_permission_mode("acceptEdits");
    let (ui, now) = scene();
    let painted = painted(80, 24, &solo(&state), &ui, now);
    assert_eq!(
        painted.coloured("? for shortcuts"),
        vec!["⏵⏵ acceptEdits".to_string()],
        "the mode is the one thing the line is allowed to colour"
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
