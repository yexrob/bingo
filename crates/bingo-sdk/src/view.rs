//! UI as data (ADR-0013): the one vocabulary a plugin describes a screen
//! with. A surface draws each node once for everyone; what it cannot draw
//! it shows as `text()`, the one fold every node ships with.
//!
//! A word this vocabulary does not have is [`View::Custom`] (ADR-0038): a
//! plugin puts up an element the sdk has no name for, and an unknown `kind`
//! off the wire lands there too, so a newer speaker never breaks an older
//! reader — it is read as its fold until the reader learns the word.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

use crate::host::Action;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", remote = "Self")]
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
    /// A word this sdk does not have (ADR-0038 §1): `kind` is the emitter's
    /// own name for the element, namespaced by its plugin (`demo.sparkline`),
    /// `data` is whatever shape that plugin documents, and `fold` is the text
    /// every surface that has not learned the kind shows instead.
    ///
    /// It is also where an unknown `kind` off the wire lands, and then `data`
    /// is the whole node as it arrived — nothing said is lost, whether or not
    /// this sdk has the word for it.
    Custom {
        #[serde(rename = "customKind")]
        kind: String,
        data: Value,
        fold: String,
    },
}

/// The tag [`View::Custom`] rides under, so that a custom node a plugin wrote
/// is a *known* kind and reads back as itself. The plugin's own word for the
/// element sits beside it in `customKind`, out of the tag's way.
const CUSTOM: &str = "custom";

/// Every word this sdk has. A `kind` that is not one of these is a word it
/// has not learned, and lands in [`View::Custom`] rather than failing the
/// whole parse (ADR-0038 §2). Pinned to the variants by the schema in
/// `the_known_kinds_are_the_vocabulary`, so a node added without a word here
/// would be caught by its own sdk.
const KNOWN: &[&str] = &[
    "text", "markdown", "code", "diff", "list", "table", "keyValue", "progress", "badge", "tree",
    "stack", "columns", "panel", "actions", CUSTOM,
];

/// The catch-all (ADR-0038 §2). A node this sdk has the word for parses
/// exactly as it always did, mistakes included; anything else is kept whole
/// and read as its fold.
impl<'de> Deserialize<'de> for View {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let node = Value::deserialize(deserializer)?;
        match unlearned(&node) {
            Some(kind) => Ok(caught(kind.to_owned(), node)),
            None => known(node).map_err(de::Error::custom),
        }
    }
}

impl Serialize for View {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Node<'a>(#[serde(with = "View")] &'a View);
        Node(self).serialize(serializer)
    }
}

/// The vocabulary's own derived reading, reached through the wrapper serde's
/// `remote` generates: one definition of the known nodes, no second spelling.
fn known(node: Value) -> Result<View, serde_json::Error> {
    #[derive(Deserialize)]
    struct Node(#[serde(with = "View")] View);
    serde_json::from_value::<Node>(node).map(|node| node.0)
}

/// The word of a node this sdk has not learned; `None` for every node it can
/// read, malformed ones included — those are still mistakes.
fn unlearned(node: &Value) -> Option<&str> {
    let kind = node.get("kind")?.as_str()?;
    (!KNOWN.contains(&kind)).then_some(kind)
}

/// A node from a newer speaker, whole: `data` keeps everything it said, so a
/// surface that learns the word later still has it, and the fold is what
/// every surface shows until then.
fn caught(kind: String, node: Value) -> View {
    let fold = match node.get("fold").and_then(Value::as_str) {
        Some(fold) => fold.to_owned(),
        None => format!("[{kind}]"),
    };
    View::Custom {
        kind,
        data: node,
        fold,
    }
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
            // The one node whose fold its author wrote (ADR-0038 §1): the sdk
            // has no word for it, so it has nothing of its own to say.
            View::Custom { fold, .. } => fold.clone(),
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

    /// The committed shape of the vocabulary, and the fence the catch-all is
    /// built against (ADR-0038 §2): a known kind reads back as itself and
    /// writes the same bytes it was read from. This snapshot moving means a
    /// reader that predates the change stopped understanding a word it knew.
    #[test]
    fn every_node_has_a_wire_shape_and_round_trips() {
        let views: Vec<View> = every_node().into_iter().map(|(v, _)| v).collect();
        insta::assert_json_snapshot!("views", views);
        for view in &views {
            let json = serde_json::to_string(view).unwrap();
            let read: View = serde_json::from_str(&json).unwrap();
            assert_eq!(&read, view);
            assert_eq!(serde_json::to_string(&read).unwrap(), json);
        }
    }

    #[test]
    fn every_node_folds_to_text() {
        for (view, fold) in every_node() {
            assert_eq!(view.fold(), fold, "{view:?}");
        }
    }

    /// The other half of the fence: a word this sdk has no name for degrades
    /// (ADR-0038 §2), but a known word spelled wrong is still a mistake, and
    /// so is a node with no word at all.
    #[test]
    fn a_known_kind_that_is_malformed_is_still_an_error() {
        for json in [
            r#"{"kind":"text"}"#,
            r#"{"kind":"table","headers":[]}"#,
            r#"{"kind":"progress","value":"a lot"}"#,
            r#"{"text":"no kind at all"}"#,
            r#"{"kind":42,"text":"a kind is a word"}"#,
            r#""not an object at all""#,
        ] {
            assert!(serde_json::from_str::<View>(json).is_err(), "{json}");
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

    fn sparkline() -> View {
        View::Custom {
            kind: "demo.sparkline".into(),
            data: serde_json::json!({"points": [3, 5, 8, 13]}),
            fold: "3 5 8 13".into(),
        }
    }

    /// The list the catch-all consults is the vocabulary itself.
    #[test]
    fn the_known_kinds_are_the_vocabulary() {
        let schema = serde_json::to_value(schemars::schema_for!(View)).unwrap();
        let kinds: Vec<&str> = schema["oneOf"]
            .as_array()
            .expect("one shape per node")
            .iter()
            .map(|node| node["properties"]["kind"]["const"].as_str().expect("a kind"))
            .collect();
        assert_eq!(kinds, KNOWN);
    }

    /// The spelling a custom node rides under: the tag says `custom`, which
    /// this sdk knows, and the plugin's own word sits beside it. That is what
    /// keeps a custom node from being caught as unknown by its own reader and
    /// wrapped a second time.
    #[test]
    fn a_custom_node_writes_the_reserved_shape_and_reads_back_as_itself() {
        let json = serde_json::to_value(sparkline()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "custom",
                "customKind": "demo.sparkline",
                "data": {"points": [3, 5, 8, 13]},
                "fold": "3 5 8 13",
            })
        );
        assert_eq!(serde_json::from_value::<View>(json).unwrap(), sparkline());
        assert_eq!(sparkline().fold(), "3 5 8 13");
    }

    /// The door ADR-0038 §2 opens: a word from a newer speaker is text here,
    /// not an error, and everything it said is still in `data`.
    #[test]
    fn a_kind_this_sdk_has_never_heard_of_lands_in_custom_whole() {
        let from_the_future = serde_json::json!({
            "kind": "chart.candles",
            "series": [1, 2],
            "fold": "AAPL 1 2",
        });
        assert_eq!(
            serde_json::from_value::<View>(from_the_future.clone()).unwrap(),
            View::Custom {
                kind: "chart.candles".into(),
                data: from_the_future,
                fold: "AAPL 1 2".into(),
            }
        );
    }

    /// A newer speaker that forgot its fold still says which word it was.
    #[test]
    fn an_unknown_kind_with_no_fold_folds_to_its_name() {
        let view: View = serde_json::from_value(serde_json::json!({"kind": "chart.candles"}))
            .expect("a word this sdk has not learned is text, not an error");
        assert_eq!(view.fold(), "[chart.candles]");
    }

    /// Both origins of a `Custom` survive a round trip, which is what lets a
    /// surface pass a node it cannot draw on to one that can.
    #[test]
    fn a_caught_node_round_trips_without_nesting_itself_again() {
        let caught: View = serde_json::from_value(serde_json::json!({
            "kind": "chart.candles",
            "series": [1, 2],
        }))
        .unwrap();
        let written = serde_json::to_string(&caught).unwrap();
        let read: View = serde_json::from_str(&written).unwrap();
        assert_eq!(read, caught);
        assert_eq!(serde_json::to_string(&read).unwrap(), written);
    }

    /// A node from the future nested inside a node this sdk knows: the parse
    /// degrades where the unknown word is and nowhere else.
    #[test]
    fn an_unknown_node_inside_a_known_one_folds_in_place() {
        let view: View = serde_json::from_value(serde_json::json!({
            "kind": "panel",
            "title": "Board",
            "child": {"kind": "chart.candles", "fold": "AAPL 1 2"},
        }))
        .expect("a panel around a word from the future is still a panel");
        assert_eq!(view.fold(), "Board\nAAPL 1 2");
    }
}
