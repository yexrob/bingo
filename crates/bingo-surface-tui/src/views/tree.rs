//! `View::Tree`: `├─ └─` with badges (design §5). The guides say which node
//! a child belongs to, so a deep tree is read without counting indents.
//!
//! It draws whole: folding a node on `←` wants a cursor and a per-node open
//! set, which is state the surface does not keep for a value it is handed
//! afresh every frame.

use bingo_sdk::TreeNode;
use ratatui::text::{Line, Span};

use crate::theme;
use crate::views::badge;

pub fn lines(nodes: &[TreeNode]) -> Vec<Line<'static>> {
    branch(nodes, "")
}

/// One level of the tree, under the guides its ancestors left.
fn branch(nodes: &[TreeNode], guides: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (at, node) in nodes.iter().enumerate() {
        let last = at + 1 == nodes.len();
        out.push(row(node, guides, last));
        let under = format!(
            "{guides}{}",
            match last {
                true => "   ".to_string(),
                false => format!("{}  ", theme::wall()),
            }
        );
        out.extend(branch(&node.children, &under));
    }
    out
}

fn row(node: &TreeNode, guides: &str, last: bool) -> Line<'static> {
    let corner = match last {
        true => theme::corner(),
        false => theme::branch(),
    };
    let mut spans = vec![
        Span::styled(format!("{guides}{corner}{} ", theme::rule()), theme::dim()),
        Span::styled(node.label.clone(), theme::text()),
    ];
    if let Some(text) = node.badge.as_ref() {
        spans.push(Span::raw(" "));
        spans.push(badge::span(text, node.tone));
    }
    Line::from(spans)
}
