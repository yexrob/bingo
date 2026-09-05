//! The screens a shell running in the background is read through (M75):
//! Claude Code's own shape, read off its screen and drawn in bingo's grammar.
//!
//! Three things are being decided here and nowhere else: that the row which
//! started a shell says where it went and, once it has gone, that it went;
//! that the status line counts the shells still running; and that the running
//! set is the one signal the rail never makes a card of.

use super::*;

const ID: &str = "job_ab12cd34";
const COMMAND: &str = "cargo test --workspace";

/// The set as the shell's plugin signals it while the one shell runs.
fn set() -> serde_json::Value {
    json!({
        "kind": "table",
        "headers": ["job", "command", "since"],
        "rows": [[ID, COMMAND, "10:22:07"]],
    })
}

/// The call that went to the background, already come back: the text the
/// model read, and beside it the kind the plugin named (ADR-0038).
fn backgrounded(seq: u64) -> bingo_sdk::Frame {
    let output = ToolOutput {
        parts: vec![ContentPart::text(format!(
            "Started `{COMMAND}` in the background as job {ID}. You will be told when it ends."
        ))],
        is_error: false,
        display: Some(View::Custom {
            kind: "job".into(),
            data: json!({ "id": ID, "command": COMMAND }),
            fold: format!("Started in the background as {ID}"),
        }),
    };
    item(
        seq,
        tool(
            "itm_2",
            "Bash",
            json!({ "command": COMMAND, "background": true }),
            Some(output),
            ItemStatus::Completed,
        ),
    )
}

/// The shell still running: the set signalled, the model's word said.
fn running() -> bingo_sdk::SessionState {
    folded(vec![
        item(1, user("itm_1", "run the tests in the background")),
        backgrounded(2),
        frame(3, signalled("bingo.tools.bash", "jobs", set())),
        item(4, assistant("itm_3", "Started.", ItemStatus::Completed)),
    ])
}

/// The same session after the shell ended: the set taken away, and the
/// completion delivered as the turn it opens.
fn ran() -> bingo_sdk::SessionState {
    folded(vec![
        item(1, user("itm_1", "run the tests in the background")),
        backgrounded(2),
        frame(3, signalled("bingo.tools.bash", "jobs", set())),
        item(4, assistant("itm_3", "Started.", ItemStatus::Completed)),
        frame(
            5,
            signalled("bingo.tools.bash", "jobs", serde_json::Value::Null),
        ),
        item(
            6,
            delivered(
                "itm_4",
                "bash",
                None,
                &format!(
                    "Background job {ID} (`{COMMAND}`) exited with code 0 after 45s.\n\
                     `BashOutput` with id \"{ID}\" reads what it wrote."
                ),
            ),
        ),
        item(
            7,
            assistant("itm_5", "The tests passed.", ItemStatus::Completed),
        ),
    ])
}

/// `⎿  Running in the background` under the call, `1 shell` on the status
/// line, and at 120 columns no card in the rail.
#[test]
fn shell_running() {
    let (ui, now) = scene();
    let state = running();
    let narrow = draw_tree(80, 24, &solo(&state), &ui, now);
    assert!(narrow.contains("⎿  Running in the background"), "{narrow}");
    assert!(narrow.contains("1 shell"), "{narrow}");
    assert!(!narrow.contains("job_ab12cd34"), "{narrow}");
    let wide = draw_tree(120, 40, &solo(&state), &ui, now);
    assert!(!wide.contains("since"), "no card: {wide}");
    both("shell_running", &solo(&state), &ui, now);
}

/// The same row reads `Ran in the background` once the set no longer lists
/// the shell, the count is gone, and the completion is a turn of its own.
#[test]
fn shell_ran() {
    let (ui, now) = scene();
    let state = ran();
    let screen = draw_tree(80, 24, &solo(&state), &ui, now);
    assert!(screen.contains("⎿  Ran in the background"), "{screen}");
    assert!(!screen.contains("Running in the background"), "{screen}");
    assert!(!screen.contains("1 shell"), "{screen}");
    assert!(screen.contains("⏺ Background job job_ab12cd34"), "{screen}");
    both("shell_ran", &solo(&state), &ui, now);
}
