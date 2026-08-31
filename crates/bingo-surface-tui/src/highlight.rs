//! Code, highlighted.
//!
//! syntect parses and [`crate::theme`] paints: a scope becomes one of three
//! inks (design §5), never a colour of its own. The regex engine is
//! `fancy-regex`, so nothing here links a C library; `onig` is banned in
//! `deny.toml`.
//!
//! An answer still arriving is the only block drawn twice, so the parser is
//! kept where the last **whole** line left it. A delta re-parses the row it
//! added and nothing above it; the half-written last row is parsed against a
//! copy of that state, so the next delta still resumes from the same place.

use std::cell::RefCell;
use std::sync::OnceLock;

use ratatui::text::{Line, Span};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::theme::{self, Ink};

/// How many blocks stay warm. One answer may carry several fences, and the
/// one being written is at the end of it; a handful covers the rest.
const WARM: usize = 8;

/// The syntaxes, read once: bat's set, which is syntect's own plus the
/// languages a person actually opens.
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// [`theme::INKS`] with its scopes resolved, once.
fn inks() -> &'static [(Ink, Scope)] {
    static INKS: OnceLock<Vec<(Ink, Scope)>> = OnceLock::new();
    INKS.get_or_init(|| {
        theme::INKS
            .iter()
            .filter_map(|(ink, path)| Scope::new(path).ok().map(|scope| (*ink, scope)))
            .collect()
    })
}

/// The syntax a fence's word names, and plain text for one nothing claims —
/// an unknown language still reads as code.
fn syntax(lang: &str) -> &'static SyntaxReference {
    let set = syntaxes();
    set.find_syntax_by_token(lang)
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

/// The rows of a code block, highlighted, one line in one line out.
pub fn lines(lang: &str, text: &str) -> Vec<Line<'static>> {
    let (whole, tail) = split(text);
    BLOCKS.with_borrow_mut(|blocks| {
        let mut block = blocks
            .iter()
            .position(|block| block.resumes(lang, whole))
            .map(|at| blocks.remove(at))
            .unwrap_or_else(|| Block::new(lang));
        block.grow(whole);
        let mut rows = block.rows.clone();
        rows.extend(block.peek(tail));
        blocks.push(block);
        if blocks.len() > WARM {
            blocks.remove(0);
        }
        rows
    })
}

/// A block's whole lines and the half-written one after them. Only whole
/// lines may advance the parser: a row that is still being typed would leave
/// it in the middle of a token.
fn split(text: &str) -> (&str, &str) {
    match text.rfind('\n') {
        Some(at) => text.split_at(at + 1),
        None => ("", text),
    }
}

thread_local! {
    /// The blocks kept warm for this thread. A memo of what [`lines`] already
    /// answered, never a second copy of what a block says: every entry is
    /// thrown away the moment its source stops being a prefix of the text.
    static BLOCKS: RefCell<Vec<Block>> = const { RefCell::new(Vec::new()) };
}

/// One code block, highlighted, with the parser as its last whole line left it.
struct Block {
    lang: String,
    /// The whole lines already highlighted, verbatim.
    source: String,
    rows: Vec<Line<'static>>,
    parse: ParseState,
    stack: ScopeStack,
}

impl Block {
    fn new(lang: &str) -> Self {
        Self {
            lang: lang.to_string(),
            source: String::new(),
            rows: Vec::new(),
            parse: ParseState::new(syntax(lang)),
            stack: ScopeStack::new(),
        }
    }

    /// Whether this block is the one being asked about and has been asked
    /// about before: same language, and what it holds opens what is asked.
    fn resumes(&self, lang: &str, whole: &str) -> bool {
        self.lang == lang && whole.starts_with(&self.source)
    }

    /// Parse whatever is new, and keep the parser where it stopped.
    fn grow(&mut self, whole: &str) {
        for line in whole[self.source.len()..].lines() {
            let row = row(line, &mut self.parse, &mut self.stack);
            self.rows.push(row);
        }
        self.source = whole.to_string();
    }

    /// The half-written last row, drawn against a copy of the parser so the
    /// kept one still stands at the end of the whole lines.
    fn peek(&self, tail: &str) -> Option<Line<'static>> {
        if tail.is_empty() {
            return None;
        }
        Some(row(tail, &mut self.parse.clone(), &mut self.stack.clone()))
    }
}

/// One line: syntect says where the scopes change, the token table says what
/// each run between two of them is drawn in.
fn row(line: &str, parse: &mut ParseState, stack: &mut ScopeStack) -> Line<'static> {
    // The syntaxes are the newline-terminated set: a line without its ending
    // would leave a rule half-matched.
    let source = format!("{line}\n");
    let ops = parse.parse_line(&source, syntaxes()).unwrap_or_default();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut at = 0usize;
    for (offset, op) in ops {
        run(&source[at..offset], stack, &mut spans);
        let _ = stack.apply(&op);
        at = offset;
    }
    run(&source[at..], stack, &mut spans);
    Line::from(spans)
}

/// One run of a line, in the ink of whatever scope is innermost, joined to the
/// run before it when the two read the same.
fn run(text: &str, stack: &ScopeStack, spans: &mut Vec<Span<'static>>) {
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        return;
    }
    let style = theme::ink(ink(stack));
    match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => spans.push(Span::styled(text.to_string(), style)),
    }
}

/// What the scope stack says a run is: the innermost scope the table knows,
/// and [`Ink::Plain`] when it knows none of them.
fn ink(stack: &ScopeStack) -> Ink {
    stack
        .scopes
        .iter()
        .rev()
        .find_map(|scope| {
            inks()
                .iter()
                .find(|(_, prefix)| prefix.is_prefix_of(*scope))
                .map(|(ink, _)| *ink)
        })
        .unwrap_or(Ink::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    /// The style each run of a row was drawn in, with its text.
    fn runs(line: &Line<'static>) -> Vec<(String, ratatui::style::Style)> {
        line.spans
            .iter()
            .map(|span| (span.content.to_string(), span.style))
            .collect()
    }

    /// Nothing kept from another test: the memo is this thread's.
    fn fresh() {
        BLOCKS.with_borrow_mut(Vec::clear);
    }

    #[test]
    fn a_line_of_rust_wears_the_three_inks_and_nothing_else() {
        fresh();
        let drawn = lines("rust", "let x = 1; // one\n");
        assert_eq!(text(&drawn), vec!["let x = 1; // one"]);
        let spent: Vec<ratatui::style::Style> = runs(&drawn[0])
            .into_iter()
            .map(|(_, style)| style)
            .collect();
        for style in &spent {
            assert!(
                [theme::mode(), theme::dim(), theme::text()].contains(style),
                "an ink outside the three: {style:?}"
            );
        }
        assert!(spent.contains(&theme::mode()), "`let` is a keyword");
        assert!(spent.contains(&theme::dim()), "the comment recedes");
    }

    #[test]
    fn a_comment_is_dim_and_a_keyword_is_the_cool_colour() {
        fresh();
        let drawn = lines("python", "# a note\ndef run():\n");
        assert_eq!(runs(&drawn[0])[0].1, theme::dim(), "{:?}", runs(&drawn[0]));
        assert_eq!(runs(&drawn[1])[0].0, "def");
        assert_eq!(runs(&drawn[1])[0].1, theme::mode());
    }

    #[test]
    fn json_keeps_its_text_and_colours_only_its_keywords() {
        fresh();
        let drawn = lines("json", "{\"live\": true, \"name\": \"bingo\"}\n");
        assert_eq!(text(&drawn), vec!["{\"live\": true, \"name\": \"bingo\"}"]);
        let coloured: Vec<String> = runs(&drawn[0])
            .into_iter()
            .filter(|(_, style)| *style == theme::mode())
            .map(|(text, _)| text)
            .collect();
        assert_eq!(coloured, vec!["true".to_string()]);
    }

    #[test]
    fn a_language_nothing_claims_still_reads_as_code() {
        fresh();
        let drawn = lines("no-such-language", "whatever this is\n");
        assert_eq!(text(&drawn), vec!["whatever this is"]);
        assert_eq!(runs(&drawn[0])[0].1, theme::text());
    }

    #[test]
    fn a_half_written_row_is_drawn_and_never_kept() {
        fresh();
        assert_eq!(
            text(&lines("rust", "fn main() {\n    let x")),
            vec!["fn main() {".to_string(), "    let x".to_string(),]
        );
        assert_eq!(
            text(&lines("rust", "fn main() {\n    let x = 1;\n")),
            vec!["fn main() {".to_string(), "    let x = 1;".to_string()],
            "the half-written row was redrawn whole, not appended to"
        );
    }

    #[test]
    fn resuming_draws_exactly_what_a_fresh_parse_would() {
        let whole = "// a module\nfn main() {\n    let x = 1;\n}\n";
        let grown = {
            fresh();
            let mut last = Vec::new();
            for end in whole.match_indices('\n').map(|(at, _)| at + 1) {
                last = lines("rust", &whole[..end]);
            }
            last
        };
        fresh();
        let once = lines("rust", whole);
        assert_eq!(
            grown.iter().map(runs).collect::<Vec<_>>(),
            once.iter().map(runs).collect::<Vec<_>>(),
        );
    }

    /// The budget of §6: a block that grew by one line costs one line.
    #[test]
    fn one_delta_re_highlights_in_under_a_millisecond() {
        fresh();
        let mut block: String = (0..400)
            .map(|i| format!("    let x{i} = {i}; // row {i}\n"))
            .collect();
        lines("rust", &block);
        block.push_str("    let last = 1;\n");
        let started = Instant::now();
        let rows = lines("rust", &block);
        let took = started.elapsed();
        assert_eq!(rows.len(), 401);
        assert!(
            took < Duration::from_millis(1),
            "one delta took {took:?}, over the frame's budget"
        );
    }

    #[test]
    fn a_block_that_changed_behind_its_last_row_is_parsed_again() {
        fresh();
        lines("rust", "let a = 1;\nlet b = 2;\n");
        assert_eq!(
            text(&lines("rust", "let c = 3;\n")),
            vec!["let c = 3;"],
            "nothing of the old block is carried into the new one"
        );
    }

    #[test]
    fn only_a_handful_of_blocks_stay_warm() {
        fresh();
        for i in 0..WARM * 2 {
            lines("rust", &format!("let x{i} = {i};\n"));
        }
        assert_eq!(BLOCKS.with_borrow(Vec::len), WARM);
    }
}
