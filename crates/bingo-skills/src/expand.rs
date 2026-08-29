//! What a skill's body becomes once the arguments are in it. Pure: one
//! left-to-right pass, so a value that itself contains `$1` is inserted as
//! text and never expanded again.

use crate::skill::Skill;

/// The variable a body uses to name its own directory. `${CLAUDE_SKILL_DIR}`
/// in Claude Code; this product is not that product.
const SKILL_DIR: &str = "${BINGO_SKILL_DIR}";

/// The placeholder for the whole argument text.
const ARGUMENTS: &str = "ARGUMENTS";

/// The body with its placeholders filled in.
///
/// `$ARGUMENTS` is everything the person or the model typed after the name;
/// `$1`…`$9` are the whitespace-separated words of it; a name declared in
/// `arguments:` is the word at its position. An indexed placeholder with no
/// word at its position is left alone, a named one becomes empty, and anything
/// else beginning with `$` is not a placeholder and is left as it is.
pub fn expand(skill: &Skill, args: &str) -> String {
    let args = args.trim();
    let values = Values::of(skill, args);
    let (mut text, received) = substitute(&skill.body, &values);
    if !received && !args.is_empty() {
        text.push_str(&appended(args));
    }
    text
}

/// Arguments a body has no placeholder for are still what was typed, so they
/// are appended rather than dropped.
fn appended(args: &str) -> String {
    format!("\n\nARGUMENTS: {args}")
}

/// What every placeholder stands for on one invocation.
struct Values<'a> {
    dir: String,
    all: &'a str,
    words: Vec<&'a str>,
    names: &'a [String],
}

/// One placeholder, resolved: how many bytes it spans and what replaces them.
struct Found {
    len: usize,
    text: String,
    /// Whether an argument reached the body through it.
    receives_argument: bool,
}

impl<'a> Values<'a> {
    fn of(skill: &'a Skill, args: &'a str) -> Self {
        Self {
            dir: skill.dir.display().to_string(),
            all: args,
            words: args.split_whitespace().collect(),
            names: &skill.argument_names,
        }
    }

    /// The placeholder at the start of `rest`, which begins with `$`.
    fn at(&self, rest: &str) -> Option<Found> {
        if rest.starts_with(SKILL_DIR) {
            return Some(Found {
                len: SKILL_DIR.len(),
                text: self.dir.clone(),
                receives_argument: false,
            });
        }
        let token = token(rest)?;
        // `$ARGUMENTS[0]` is an indexed form this product does not read; left
        // whole, it stays legible instead of becoming the arguments and `[0]`.
        if rest[1 + token.len()..].starts_with('[') {
            return None;
        }
        self.value(token).map(|text| Found {
            len: 1 + token.len(),
            text,
            receives_argument: true,
        })
    }

    /// What one `$token` stands for, or `None` when nothing does.
    fn value(&self, token: &str) -> Option<String> {
        if token == ARGUMENTS {
            return Some(self.all.to_string());
        }
        if let Some(index) = position(token) {
            return self.words.get(index).map(|word| word.to_string());
        }
        let index = self.names.iter().position(|name| name == token)?;
        Some(
            self.words
                .get(index)
                .copied()
                .unwrap_or_default()
                .to_string(),
        )
    }
}

/// The identifier after a `$`, or `None` when there is none.
fn token(rest: &str) -> Option<&str> {
    let after = &rest[1..];
    let end = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after.len());
    Some(&after[..end]).filter(|token| !token.is_empty())
}

/// `$1` is the first word, so the index is one less. `$0` names nothing.
fn position(token: &str) -> Option<usize> {
    token.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
}

/// One pass over the body, and whether any argument reached it.
fn substitute(body: &str, values: &Values<'_>) -> (String, bool) {
    let mut out = String::with_capacity(body.len());
    let mut received = false;
    let mut rest = body;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match values.at(rest) {
            Some(found) => {
                out.push_str(&found.text);
                received |= found.receives_argument;
                rest = &rest[found.len..];
            }
            None => {
                out.push('$');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    (out, received)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn skill(body: &str) -> Skill {
        Skill::parse("t", PathBuf::from("/skills/t"), body)
    }

    fn named(body: &str, names: &str) -> Skill {
        Skill::parse(
            "t",
            PathBuf::from("/skills/t"),
            &format!("---\narguments: {names}\n---\n{body}"),
        )
    }

    /// The substitution table, one row per rule.
    #[test]
    fn the_substitution_table() {
        let rows: [(&str, &str, &str); 10] = [
            ("all: $ARGUMENTS", "a b c", "all: a b c"),
            ("first: $1", "a b c", "first: a"),
            ("third: $3", "a b c", "third: c"),
            ("missing: $4", "a b c", "missing: $4\n\nARGUMENTS: a b c"),
            ("zero: $0", "a b", "zero: $0\n\nARGUMENTS: a b"),
            (
                "dir: ${BINGO_SKILL_DIR}/run.sh",
                "",
                "dir: /skills/t/run.sh",
            ),
            (
                "unknown: $HOME stays",
                "a",
                "unknown: $HOME stays\n\nARGUMENTS: a",
            ),
            (
                "indexed: $ARGUMENTS[0]",
                "a",
                "indexed: $ARGUMENTS[0]\n\nARGUMENTS: a",
            ),
            ("bare $ and $ again", "", "bare $ and $ again"),
            ("empty: [$ARGUMENTS]", "", "empty: []"),
        ];
        for (body, args, want) in rows {
            assert_eq!(expand(&skill(body), args), want, "body {body:?}");
        }
    }

    #[test]
    fn a_declared_name_is_the_word_at_its_position() {
        let s = named("Fix $issue on $branch.", "[issue, branch]");
        assert_eq!(expand(&s, "42 main"), "Fix 42 on main.");
    }

    #[test]
    fn a_declared_name_with_no_word_at_its_position_becomes_empty() {
        let s = named("Fix $issue on $branch.", "[issue, branch]");
        assert_eq!(expand(&s, "42"), "Fix 42 on .");
    }

    #[test]
    fn a_value_that_looks_like_a_placeholder_is_inserted_as_text() {
        assert_eq!(
            expand(&skill("Summarise $1"), "$ARGUMENTS"),
            "Summarise $ARGUMENTS"
        );
    }

    #[test]
    fn arguments_no_placeholder_asked_for_are_appended_not_dropped() {
        assert_eq!(
            expand(&skill("Do the thing."), "with care"),
            "Do the thing.\n\nARGUMENTS: with care"
        );
    }

    #[test]
    fn a_body_that_used_its_arguments_gets_nothing_appended() {
        assert_eq!(expand(&skill("Do $1."), "now"), "Do now.");
    }

    #[test]
    fn an_indexed_placeholder_that_matched_nothing_does_not_count_as_used() {
        assert_eq!(
            expand(&skill("Do $2."), "now"),
            "Do $2.\n\nARGUMENTS: now",
            "the word the person typed must reach the model somehow"
        );
    }

    #[test]
    fn a_named_placeholder_counts_even_when_it_expands_to_nothing() {
        let s = named("Fix $issue.", "[issue]");
        assert_eq!(expand(&s, ""), "Fix .");
    }

    #[test]
    fn a_placeholder_glued_to_a_word_is_not_a_placeholder() {
        assert_eq!(expand(&skill("$1abc"), "x"), "$1abc\n\nARGUMENTS: x");
        assert_eq!(
            expand(&skill("$ARGUMENTSX"), "x"),
            "$ARGUMENTSX\n\nARGUMENTS: x"
        );
    }

    #[test]
    fn a_bundled_skill_has_no_directory_to_name() {
        let s = Skill::parse("guide", PathBuf::new(), "at [${BINGO_SKILL_DIR}]");
        assert_eq!(expand(&s, ""), "at []");
    }

    #[test]
    fn the_whole_argument_text_keeps_its_inner_spacing() {
        assert_eq!(
            expand(&skill("$ARGUMENTS"), "  two   words  "),
            "two   words"
        );
    }
}
