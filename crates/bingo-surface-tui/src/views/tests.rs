//! Every node of the vocabulary, drawn once and read as a screen (ADR-0013
//! §1, design §9). A node enters the TUI with its screen here or it does not
//! enter: [`named`] is an exhaustive match, so a fifteenth node would not
//! compile until somebody had looked at it.

use bingo_sdk::{Action, ActionItem, Tone, TreeNode, View};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;

use super::*;

/// One node on a screen of its own, as many rows as it draws.
fn screen(view: &View, width: u16) -> String {
    let lines = render(view, usize::from(width));
    let height = u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
        .expect("a drawn frame");
    terminal.backend().to_string()
}

/// The width every rule in design §5 is written for.
fn at_80(view: &View) -> String {
    screen(view, 80)
}

fn action(name: &str) -> Action {
    Action {
        name: name.into(),
        args: serde_json::Value::Null,
    }
}

fn table() -> View {
    View::Table {
        headers: vec!["id".into(), "task".into(), "tokens".into()],
        rows: vec![
            vec!["1".into(), "write the plan".into(), "1200".into()],
            vec!["2".into(), "ship it".into()],
        ],
    }
}

fn actions() -> View {
    View::Actions {
        items: vec![
            ActionItem {
                label: "Approve".into(),
                action: action("board.tick"),
                key: None,
            },
            ActionItem {
                label: "Next".into(),
                action: action("board.next"),
                key: None,
            },
            ActionItem {
                label: "Skip".into(),
                action: action("board.skip"),
                key: Some('s'),
            },
        ],
    }
}

fn tree() -> View {
    View::Tree {
        nodes: vec![TreeNode {
            label: "project".into(),
            badge: None,
            tone: Tone::Neutral,
            children: vec![
                TreeNode {
                    label: "reviewer".into(),
                    badge: Some("waiting".into()),
                    tone: Tone::Attention,
                    children: Vec::new(),
                },
                TreeNode {
                    label: "scout".into(),
                    badge: Some("done".into()),
                    tone: Tone::Good,
                    children: vec![TreeNode {
                        label: "Read(Cargo.toml)".into(),
                        badge: None,
                        tone: Tone::Neutral,
                        children: Vec::new(),
                    }],
                },
            ],
        }],
    }
}

fn columns() -> View {
    View::Columns {
        children: vec![
            View::KeyValue {
                rows: vec![
                    ("model".into(), "gpt-5.4".into()),
                    ("rounds".into(), "12".into()),
                ],
            },
            View::List {
                items: vec!["one".into(), "two".into(), "three".into()],
            },
        ],
    }
}

/// Every node, once. The match is exhaustive on purpose.
fn named(view: &View) -> &'static str {
    match view {
        View::Text { .. } => "text",
        View::Markdown { .. } => "markdown",
        View::Code { .. } => "code",
        View::Diff { .. } => "diff",
        View::List { .. } => "list",
        View::Table { .. } => "table",
        View::KeyValue { .. } => "keyvalue",
        View::Progress { .. } => "progress",
        View::Badge { .. } => "badge",
        View::Tree { .. } => "tree",
        View::Stack { .. } => "stack",
        View::Columns { .. } => "columns",
        View::Panel { .. } => "panel",
        View::Actions { .. } => "actions",
    }
}

fn every_node() -> Vec<View> {
    vec![
        View::text("what a plugin wrote"),
        View::Markdown {
            text: "# Title\n\nbody with **weight** and a [link](https://x.dev)".into(),
        },
        View::Code {
            lang: Some("rust".into()),
            text: (1..=10)
                .map(|i| format!("let line_{i} = {i};\n"))
                .collect::<String>(),
        },
        View::Diff {
            unified: "@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n ok\n".into(),
        },
        View::List {
            items: vec!["write the plan".into(), "ship it".into()],
        },
        table(),
        View::KeyValue {
            rows: vec![
                ("model".into(), "gpt-5.4".into()),
                ("rounds".into(), "12".into()),
                ("branch".into(), String::new()),
            ],
        },
        View::Progress {
            value: 24,
            total: Some(30),
            label: Some("cargo test".into()),
        },
        View::Badge {
            text: "needs you".into(),
            tone: Tone::Attention,
        },
        tree(),
        View::Stack {
            children: vec![View::text("above"), View::text(""), table()],
        },
        columns(),
        View::Panel {
            title: "Board".into(),
            child: Box::new(table()),
        },
        actions(),
    ]
}

#[test]
fn every_node_of_the_vocabulary_has_a_screen_of_its_own() {
    let mut names: Vec<&str> = every_node().iter().map(named).collect();
    let all = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all, "one screen per node, and no node twice");
    for view in every_node() {
        insta::assert_snapshot!(named(&view), at_80(&view));
    }
}

/// Two columns of thirty cells read worse than one of sixty, so below the
/// narrow width they stack instead (design §7's readable widths).
#[test]
fn columns_stack_below_sixty_cells() {
    insta::assert_snapshot!("columns_at_50", screen(&columns(), 50));
}

#[test]
fn an_unbounded_progress_shows_its_count_and_not_a_fraction() {
    let view = View::Progress {
        value: 7,
        total: None,
        label: Some("indexing".into()),
    };
    assert_eq!(
        render(&view, 80)[0].to_string().trim_end(),
        "███░░░░░░░ 7 · indexing"
    );
}

#[test]
fn a_missing_cell_is_a_dash_and_numbers_hug_their_right_edge() {
    let drawn: Vec<String> = render(&table(), 80)
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect();
    assert_eq!(drawn[0], "id  task            tokens");
    assert_eq!(drawn[2], " 1  write the plan    1200");
    assert_eq!(
        drawn[3], " 2  ship it              –",
        "a row that is short"
    );
}

#[test]
fn a_row_wider_than_the_frame_is_cut_and_says_so() {
    let view = View::Table {
        headers: vec!["path".into()],
        rows: vec![vec!["a".repeat(40)]],
    };
    let drawn = render(&view, 20);
    assert_eq!(drawn[2].to_string(), format!("{}…", "a".repeat(19)));
}

#[test]
fn a_key_is_the_plugins_hint_else_where_the_button_sits() {
    let View::Actions { items } = actions() else {
        panic!("actions");
    };
    let offered: Vec<&ActionItem> = items.iter().collect();
    assert_eq!(actions::key_of(&items[0], 0), '1');
    assert_eq!(actions::key_of(&items[2], 2), 's', "the plugin named one");
    assert_eq!(
        actions::fired(&offered, '2').map(|item| item.label.as_str()),
        Some("Next")
    );
    assert_eq!(actions::fired(&offered, '9'), None);
}

#[test]
fn a_fired_button_wears_the_mark_until_the_answer_comes_back() {
    let marks = Marks {
        pending: Some(action("board.tick")),
    };
    let drawn = marked(&actions(), 80, &marks)[0].to_string();
    assert!(drawn.contains("[ 1 Approve… ]"), "{drawn}");
    assert!(drawn.contains("[ 2 Next ]"), "{drawn}");
}

#[test]
fn the_actions_of_a_view_are_found_however_deep_they_sit() {
    let nested = View::Panel {
        title: "Board".into(),
        child: Box::new(View::Stack {
            children: vec![table(), actions()],
        }),
    };
    let found: Vec<&str> = actions_of(&nested)
        .iter()
        .map(|item| item.action.name.as_str())
        .collect();
    assert_eq!(found, ["board.tick", "board.next", "board.skip"]);
    assert!(actions_of(&table()).is_empty());
}

#[test]
fn buttons_that_do_not_fit_the_width_go_on_the_next_row() {
    let drawn = render(&actions(), 24);
    assert_eq!(drawn.len(), 2, "{drawn:?}");
}
