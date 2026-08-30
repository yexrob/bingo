//! UI as data (ADR-0013): the one vocabulary a plugin describes a screen
//! with. A surface draws each node once for everyone; what it cannot draw
//! it shows as `text()`, the one fold every node ships with.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::host::Action;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum View {
    Text {
        text: String,
    },
    Markdown {
        text: String,
    },
    Code {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
        text: String,
    },
    Diff {
        unified: String,
    },
    List {
        items: Vec<String>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    KeyValue {
        rows: Vec<(String, String)>,
    },
    Progress {
        value: u64,
        /// Unbounded when absent: the surface shows activity, not a fraction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Badge {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    Tree {
        nodes: Vec<TreeNode>,
    },
    Stack {
        children: Vec<View>,
    },
    Columns {
        children: Vec<View>,
    },
    Panel {
        title: String,
        child: Box<View>,
    },
    Actions {
        items: Vec<ActionItem>,
    },
}

/// The one styling hook a plugin has; the surface owns the colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    #[default]
    Neutral,
    Good,
    Bad,
    /// Wants a person: a surface makes it move.
    Attention,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default)]
    pub tone: Tone,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

/// A button: the surface fires `Input::Action` with `action` (ADR-0008 §1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub label: String,
    pub action: Action,
    /// A single-key hint; the surface may use another key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<char>,
}

impl View {
    pub fn text(text: impl Into<String>) -> Self {
        View::Text { text: text.into() }
    }

    /// The degrade: what `--print`, an IM channel and a surface that cannot
    /// draw a node show instead.
    pub fn fold(&self) -> String {
        match self {
            View::Text { text } | View::Markdown { text } | View::Code { text, .. } => text.clone(),
            View::Diff { unified } => unified.clone(),
            View::List { items } => lines(items.iter().map(|item| format!("- {item}"))),
            View::Table { headers, rows } => lines(
                std::iter::once(headers)
                    .chain(rows)
                    .map(|row| row.join(" · ")),
            ),
            View::KeyValue { rows } => lines(rows.iter().map(|(k, v)| format!("{k}: {v}"))),
            View::Progress {
                value,
                total,
                label,
            } => progress(*value, *total, label.as_deref()),
            View::Badge { text, .. } => format!("[{text}]"),
            View::Tree { nodes } => lines(nodes.iter().flat_map(|node| node.fold(0))),
            View::Stack { children } | View::Columns { children } => {
                lines(children.iter().map(View::fold))
            }
            View::Panel { title, child } => format!("{title}\n{}", child.fold()),
            View::Actions { items } => items
                .iter()
                .map(|item| format!("[{}]", item.label))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

impl TreeNode {
    fn fold(&self, depth: usize) -> Vec<String> {
        let badge = self
            .badge
            .as_ref()
            .map(|badge| format!(" [{badge}]"))
            .unwrap_or_default();
        let mut out = vec![format!("{}{}{badge}", "  ".repeat(depth), self.label)];
        out.extend(self.children.iter().flat_map(|child| child.fold(depth + 1)));
        out
    }
}

fn lines(rows: impl Iterator<Item = String>) -> String {
    rows.collect::<Vec<_>>().join("\n")
}

fn progress(value: u64, total: Option<u64>, label: Option<&str>) -> String {
    let amount = match total {
        Some(total) if total > 0 => format!("{} %", value * 100 / total),
        _ => value.to_string(),
    };
    match label {
        Some(label) => format!("{label} {amount}"),
        None => amount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(name: &str) -> Action {
        Action {
            name: name.into(),
            args: serde_json::Value::Null,
        }
    }

    /// Every node once: the wire shape and the fold are the contract.
    fn every_node() -> Vec<(View, &'static str)> {
        vec![
            (View::text("plain"), "plain"),
            (
                View::Markdown {
                    text: "# Title\n\nbody".into(),
                },
                "# Title\n\nbody",
            ),
            (
                View::Code {
                    lang: Some("rust".into()),
                    text: "fn main() {}".into(),
                },
                "fn main() {}",
            ),
            (
                View::Diff {
                    unified: "@@ -1 +1 @@\n-a\n+b\n".into(),
                },
                "@@ -1 +1 @@\n-a\n+b\n",
            ),
            (
                View::List {
                    items: vec!["one".into(), "two".into()],
                },
                "- one\n- two",
            ),
            (
                View::Table {
                    headers: vec!["name".into(), "state".into()],
                    rows: vec![vec!["reviewer".into(), "running".into()]],
                },
                "name · state\nreviewer · running",
            ),
            (
                View::KeyValue {
                    rows: vec![("model".into(), "gpt-5.4".into())],
                },
                "model: gpt-5.4",
            ),
            (
                View::Progress {
                    value: 24,
                    total: Some(30),
                    label: Some("cargo test".into()),
                },
                "cargo test 80 %",
            ),
            (
                View::Badge {
                    text: "needs you".into(),
                    tone: Tone::Attention,
                },
                "[needs you]",
            ),
            (
                View::Tree {
                    nodes: vec![TreeNode {
                        label: "project".into(),
                        badge: None,
                        tone: Tone::Neutral,
                        children: vec![TreeNode {
                            label: "reviewer".into(),
                            badge: Some("waiting".into()),
                            tone: Tone::Attention,
                            children: Vec::new(),
                        }],
                    }],
                },
                "project\n  reviewer [waiting]",
            ),
            (
                View::Stack {
                    children: vec![View::text("a"), View::text("b")],
                },
                "a\nb",
            ),
            (
                View::Columns {
                    children: vec![View::text("left"), View::text("right")],
                },
                "left\nright",
            ),
            (
                View::Panel {
                    title: "Board".into(),
                    child: Box::new(View::text("empty")),
                },
                "Board\nempty",
            ),
            (
                View::Actions {
                    items: vec![
                        ActionItem {
                            label: "Approve".into(),
                            action: action("board.tick"),
                            key: Some('1'),
                        },
                        ActionItem {
                            label: "Next".into(),
                            action: action("board.next"),
                            key: None,
                        },
                    ],
                },
                "[Approve] [Next]",
            ),
        ]
    }

    #[test]
    fn every_node_has_a_wire_shape_and_round_trips() {
        let views: Vec<View> = every_node().into_iter().map(|(v, _)| v).collect();
        insta::assert_json_snapshot!("views", views);
        for view in &views {
            let json = serde_json::to_string(view).unwrap();
            assert_eq!(&serde_json::from_str::<View>(&json).unwrap(), view);
        }
    }

    #[test]
    fn every_node_folds_to_text() {
        for (view, fold) in every_node() {
            assert_eq!(view.fold(), fold, "{view:?}");
        }
    }

    #[test]
    fn an_unbounded_progress_folds_to_its_count() {
        let view = View::Progress {
            value: 7,
            total: None,
            label: None,
        };
        assert_eq!(view.fold(), "7");
    }
}
