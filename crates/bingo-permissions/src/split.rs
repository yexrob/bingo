//! One Bash string, every command it will actually run.
//!
//! A permission rule that prefix-matches the whole string is not a rule at
//! all: `Bash(ls)` would cover `ls; rm -rf ~`. So the grammar does the
//! splitting — `&&`, `||`, `;`, `|`, `&`, subshells, command and process
//! substitutions and redirections all surface the commands inside them.
//!
//! A parse carrying an `ERROR` or `MISSING` node is not trusted at all: what
//! the shell would run cannot be read off it, so nothing may be allowed on its
//! strength (`Split::is_parsed`).

use std::collections::VecDeque;

use tree_sitter::{Node, Tree};

/// Node kinds that stand for one command the shell runs.
const COMMAND_KINDS: &[&str] = &[
    "command",
    "declaration_command",
    "unset_command",
    "test_command",
];

/// The commands inside one Bash string, and whether the parse can be trusted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Split {
    parts: Vec<String>,
    parsed: bool,
}

impl Split {
    /// Every command the string runs, in source order. Nested commands appear
    /// alongside the command that contains them, so `echo $(rm -rf /)` yields
    /// both — an allow rule has to cover the `rm` too.
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    /// False when the parse carried an `ERROR`/`MISSING` node or the parser
    /// could not be built. Allow rules never hold over an untrusted split.
    pub fn is_parsed(&self) -> bool {
        self.parsed
    }

    /// One command, no operators — the only shape a session-scoped rule may be
    /// derived from, because a prefix of the first command says nothing about
    /// the rest of a compound one.
    pub fn is_simple(&self) -> bool {
        self.parsed && self.parts.len() == 1
    }

    /// The first token of a simple command; the head a `Bash(head:*)` rule is
    /// built from.
    pub fn head(&self) -> Option<&str> {
        let single = match self.parts.as_slice() {
            [one] if self.parsed => one.as_str(),
            _ => return None,
        };
        single.split_whitespace().next()
    }
}

pub fn split(command: &str) -> Split {
    let Some(tree) = parse(command) else {
        return Split {
            parts: Vec::new(),
            parsed: false,
        };
    };
    let root = tree.root_node();
    Split {
        parts: collect(root, command),
        parsed: !root.has_error(),
    }
}

fn parse(command: &str) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    parser.parse(command, None)
}

/// Breadth-first and iterative on purpose: a nesting depth chosen by the model
/// must not be able to overflow the stack of the process deciding whether to
/// trust it. A containing command is therefore seen before the commands it
/// substitutes.
fn collect(root: Node<'_>, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([root]);
    while let Some(node) = queue.pop_front() {
        if COMMAND_KINDS.contains(&node.kind()) {
            push(span(node), source, &mut out);
        }
        let mut cursor = node.walk();
        queue.extend(node.children(&mut cursor));
    }
    out
}

/// A redirected command runs with its redirections, and `cat x > /etc/passwd`
/// is a different act from `cat x`; the rule has to see the whole statement.
fn span(node: Node<'_>) -> Node<'_> {
    let Some(parent) = node.parent() else {
        return node;
    };
    let redirected = parent.kind() == "redirected_statement"
        && parent
            .child_by_field_name("body")
            .is_some_and(|b| b == node);
    if redirected { parent } else { node }
}

fn push(node: Node<'_>, source: &str, out: &mut Vec<String>) {
    let Some(text) = source.get(node.byte_range()) else {
        return;
    };
    let text = text.trim();
    if !text.is_empty() {
        out.push(text.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(command: &str) -> Vec<String> {
        split(command).parts().to_vec()
    }

    #[test]
    fn a_simple_command_is_one_part() {
        let split = split("cargo test --locked");
        assert_eq!(split.parts(), ["cargo test --locked"]);
        assert!(split.is_parsed() && split.is_simple());
        assert_eq!(split.head(), Some("cargo"));
    }

    #[test]
    fn every_sequencing_operator_splits() {
        for command in [
            "cd /tmp && rm -rf /",
            "true || rm -rf /",
            "ls; rm -rf ~",
            "cat x | rm -rf /",
            "ls & rm -rf /",
            "echo hi\nrm -rf /",
        ] {
            let split = split(command);
            assert!(split.is_parsed(), "{command}");
            assert_eq!(split.parts().len(), 2, "{command}: {:?}", split.parts());
            assert!(!split.is_simple(), "{command}");
        }
    }

    #[test]
    fn subshells_and_substitutions_yield_their_inner_commands() {
        assert!(parts("(cd /tmp && rm -rf /)").contains(&"rm -rf /".to_string()));
        assert!(parts("echo $(rm -rf /)").contains(&"rm -rf /".to_string()));
        assert!(parts("echo `rm -rf /`").contains(&"rm -rf /".to_string()));
        assert!(parts("diff <(sort a) <(sort b)").contains(&"sort a".to_string()));
        assert!(parts("for f in *; do rm $f; done").contains(&"rm $f".to_string()));
        assert!(parts("if true; then rm -rf /; fi").contains(&"rm -rf /".to_string()));
    }

    #[test]
    fn a_substituted_command_keeps_the_command_that_contains_it() {
        assert_eq!(parts("echo $(rm -rf /)"), ["echo $(rm -rf /)", "rm -rf /"]);
    }

    #[test]
    fn a_redirection_stays_with_its_command() {
        assert_eq!(parts("cat x > /etc/passwd"), ["cat x > /etc/passwd"]);
        assert!(split("cat x > /etc/passwd").is_simple());
        // A leading redirect is already inside the command node.
        assert_eq!(parts("> /etc/passwd cat x"), ["> /etc/passwd cat x"]);
    }

    #[test]
    fn quoted_separators_are_not_operators() {
        assert_eq!(parts("echo 'a; b'"), ["echo 'a; b'"]);
        assert_eq!(parts("echo \"a && b\""), ["echo \"a && b\""]);
    }

    #[test]
    fn an_unterminated_quote_is_never_trusted() {
        let split = split("ls \"; rm -rf ~");
        assert!(!split.is_parsed());
        assert!(!split.is_simple());
        assert_eq!(split.head(), None);
    }

    #[test]
    fn nothing_to_run_is_no_part_at_all() {
        for command in ["", "   ", "# just a comment"] {
            let split = split(command);
            assert!(split.is_parsed(), "{command:?}");
            assert!(split.parts().is_empty(), "{command:?}");
            assert!(!split.is_simple(), "{command:?}");
        }
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let depth = 2_000;
        let command = format!("{}echo hi{}", "$(".repeat(depth), ")".repeat(depth));
        let split = split(&command);
        assert!(!split.parts().is_empty());
    }
}
