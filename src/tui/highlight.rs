//! Syntax highlighting for fenced code blocks.
//!
//! Two deliberate narrowings sit between [`synoptic`] and the renderer:
//!
//! 1. **A closed language list.** `synoptic::from_extension` answers `Some` for
//!    *every* string — an unknown extension yields an empty highlighter that
//!    silently tokenizes nothing. Guessing a language from a tag we do not know
//!    is worse than not colouring it, so [`language_for`] is an explicit
//!    allowlist and an unrecognised fence stays monochrome.
//! 2. **A closed colour vocabulary.** synoptic emits ~30 token names that vary
//!    by grammar; we fold them into the eight [`Class`]es below and colour those
//!    from the active [`Theme`]. The palette therefore follows `/theme` for
//!    free, and no bundled editor theme gets to argue with ours.
//!
//! The result is quiet on purpose: comments recede to the muted tier, keywords
//! borrow the accent, strings and literals get one hue each. Code in a reply is
//! something to read, not a colour wheel.
//!
//! # Cost
//!
//! Highlighting happens where rows are built, and rows are built more than once
//! only for the streaming tail. A bounded memo keyed on `(language, source)`
//! makes a repeat call free, so a block that is re-rendered without changing —
//! every frame of a live tail, every rebuild of a cached block — pays nothing.
//! A block that *is* still growing pays one pass per change, which is the same
//! order as the markdown re-parse it arrives with. Blocks past
//! [`MAX_HIGHLIGHT_BYTES`] or [`MAX_HIGHLIGHT_LINES`] are handed back
//! unhighlighted rather than allowed to make a frame late.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use ratatui::style::Color;

use crate::tui::theme::Theme;

/// Above this many bytes a block is left monochrome: past it the pass stops
/// being free and starts being a dropped frame.
const MAX_HIGHLIGHT_BYTES: usize = 96 * 1024;

/// Above this many lines a block is left monochrome (see
/// [`MAX_HIGHLIGHT_BYTES`]).
const MAX_HIGHLIGHT_LINES: usize = 4_000;

/// How many distinct blocks the memo keeps before it starts forgetting the
/// oldest. Sized for a long transcript's worth of visible code, not a session's.
const MEMO_CAPACITY: usize = 256;

/// The colour vocabulary of a code block.
///
/// Deliberately smaller than synoptic's token set: a reader distinguishes
/// "comment / string / keyword" at a glance and nothing finer, so more classes
/// would buy noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Prose the compiler ignores.
    Comment,
    /// String and character literals.
    Str,
    /// Language keywords.
    Keyword,
    /// Numbers and booleans.
    Literal,
    /// Call and definition names.
    Function,
    /// Type names, structs, classes, tags.
    Type,
    /// Attributes, macros, namespaces, config keys.
    Meta,
    /// Operators and punctuation that a grammar bothered to name.
    Operator,
    /// Everything else — the block's own foreground.
    Plain,
}

impl Class {
    /// The class a synoptic token name belongs to.
    ///
    /// Unknown names fall to [`Class::Keyword`]: synoptic's `add_keywords`
    /// helper labels every language keyword `keyword`, and the names that are
    /// *not* in the lists below are grammar-specific keyword-ish tokens, so
    /// keyword is the honest default rather than a silent drop.
    fn of(name: &str) -> Self {
        match name {
            "comment" => Self::Comment,
            "string" | "character" => Self::Str,
            "digit" | "boolean" => Self::Literal,
            "function" => Self::Function,
            "struct" | "tag" | "type" => Self::Type,
            "attribute" | "macro" | "namespace" | "key" | "reference" | "link" | "image" => {
                Self::Meta
            }
            "operator" | "linebreak" => Self::Operator,
            // The diff grammar's own two tokens read as add/remove elsewhere in
            // the UI; inside a fence they are just structure.
            "insertion" => Self::Str,
            "deletion" => Self::Keyword,
            _ => Self::Keyword,
        }
    }

    /// This class's colour under `theme`.
    ///
    /// Every colour is an existing palette token, which is the whole trick: the
    /// dark and light highlight palettes are not two more tables to maintain,
    /// they are whatever the two themes already say these tokens are.
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Comment => theme.text_muted,
            Self::Str => theme.success,
            Self::Keyword => theme.claude,
            Self::Literal => theme.math,
            Self::Function => theme.link,
            Self::Type => theme.tool_running,
            Self::Meta => theme.code_fg,
            Self::Operator => theme.text_secondary,
            Self::Plain => theme.code_block_fg,
        }
    }
}

/// One highlighted run of text within a line.
pub type Span = (Class, String);

/// A highlighted block: one `Vec<Span>` per source line.
pub type Highlighted = Rc<Vec<Vec<Span>>>;

/// A language we are willing to colour, and the synoptic extension that does it.
///
/// The tags are what people actually type after a fence; the extension is
/// synoptic's key for the same grammar.
fn language_for(lang: &str) -> Option<&'static str> {
    let tag = lang.trim().to_ascii_lowercase();
    // A fence like ```rust,ignore or ```js {highlight} names its language first.
    let tag = tag
        .split([',', ' ', '{', ':'])
        .next()
        .unwrap_or_default()
        .trim();
    Some(match tag {
        "rust" | "rs" => "rs",
        "python" | "py" | "python3" => "py",
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node" => "js",
        "typescript" | "ts" | "tsx" => "ts",
        "json" | "jsonc" => "json",
        "bash" | "sh" | "shell" | "zsh" | "console" => "sh",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "markdown" | "md" => "md",
        "diff" | "patch" => "diff",
        // Beyond the D92 minimum, but the grammars ship with synoptic anyway and
        // a monochrome C file is not a decision anyone made.
        "c" | "h" => "c",
        "cpp" | "c++" | "cxx" | "hpp" => "cpp",
        "go" | "golang" => "go",
        "java" => "java",
        "css" => "css",
        "html" => "html",
        "sql" => "sql",
        "xml" => "xml",
        "lua" => "lua",
        "ruby" | "rb" => "rb",
        _ => return None,
    })
}

thread_local! {
    /// `(language, source) -> highlighted`, oldest-first insertion order.
    ///
    /// Thread-local rather than a lock: rows are built on the UI thread, and a
    /// mutex here would buy contention we do not have in exchange for sharing we
    /// do not want (each test thread gets its own clean memo).
    static MEMO: RefCell<(HashMap<u64, Highlighted>, Vec<u64>)> =
        RefCell::new((HashMap::new(), Vec::new()));
}

/// Highlight `text` as `lang`.
///
/// Returns `None` — meaning "render this monochrome" — for an empty or unknown
/// language tag and for blocks past the size guards. Never fails otherwise: a
/// half-arrived block is highlighted as far as it has arrived, and the result
/// for the settled block is the same as if it had arrived all at once.
pub fn highlight(lang: &str, text: &str) -> Option<Highlighted> {
    let ext = language_for(lang)?;
    if text.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let key = {
        let mut h = DefaultHasher::new();
        ext.hash(&mut h);
        text.hash(&mut h);
        h.finish()
    };
    if let Some(hit) = MEMO.with(|m| m.borrow().0.get(&key).cloned()) {
        return Some(hit);
    }
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if lines.len() > MAX_HIGHLIGHT_LINES {
        return None;
    }
    let mut highlighter = synoptic::from_extension(ext, 4)?;
    highlighter.run(&lines);
    let out: Vec<Vec<Span>> = lines
        .iter()
        .enumerate()
        .map(|(y, line)| {
            highlighter
                .line(y, line)
                .into_iter()
                .map(|tok| match tok {
                    synoptic::TokOpt::Some(text, name) => (Class::of(&name), text),
                    synoptic::TokOpt::None(text) => (Class::Plain, text),
                })
                .collect()
        })
        .collect();
    let out: Highlighted = Rc::new(out);
    MEMO.with(|m| {
        let (map, order) = &mut *m.borrow_mut();
        if map.insert(key, out.clone()).is_none() {
            order.push(key);
        }
        while order.len() > MEMO_CAPACITY {
            let oldest = order.remove(0);
            map.remove(&oldest);
        }
    });
    Some(out)
}

/// Expand tabs the way [`highlight`] does, so a monochrome fence and a
/// highlighted one occupy the same columns.
///
/// synoptic renders a tab as `tab_width` spaces before it tokenizes; a fallback
/// path that passed the raw `\t` through would put the two renderings on
/// different grids, and a terminal's own tab stop is not ours to guess.
pub fn expand_tabs(line: &str) -> String {
    if line.contains('\t') {
        line.replace('\t', "    ")
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(lang: &str, text: &str) -> Vec<Vec<Class>> {
        highlight(lang, text)
            .map(|h| {
                h.iter()
                    .map(|line| line.iter().map(|(c, _)| *c).collect())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The text is reassembled verbatim: highlighting colours code, it never
    /// edits it.
    fn rejoin(lang: &str, text: &str) -> String {
        let h = highlight(lang, text).expect("supported language");
        h.iter()
            .map(|line| line.iter().map(|(_, t)| t.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn rust_fence_separates_keyword_string_and_comment() {
        let h = highlight("rust", "// note\nlet s = \"hi\";").expect("rust is supported");
        let mut by_class = HashMap::new();
        for line in h.iter() {
            for (class, text) in line {
                by_class.entry(*class).or_insert_with(Vec::new).push(text);
            }
        }
        assert!(
            by_class.contains_key(&Class::Comment),
            "`// note` is a comment, got {by_class:?}"
        );
        assert!(
            by_class.contains_key(&Class::Str),
            "`\"hi\"` is a string, got {by_class:?}"
        );
        assert!(
            by_class.contains_key(&Class::Keyword),
            "`let` is a keyword, got {by_class:?}"
        );
        // The three must be visually distinct or the classification bought
        // nothing.
        let theme = Theme::dark();
        let comment = Class::Comment.color(&theme);
        let string = Class::Str.color(&theme);
        let keyword = Class::Keyword.color(&theme);
        assert_ne!(comment, string);
        assert_ne!(string, keyword);
        assert_ne!(keyword, comment);
    }

    #[test]
    fn unknown_and_empty_languages_stay_monochrome() {
        assert!(highlight("", "let x = 1;").is_none(), "empty tag");
        assert!(highlight("brainfuck", "+++.").is_none(), "unknown tag");
        assert!(highlight("   ", "x").is_none(), "blank tag");
        assert!(language_for("wat").is_none());
        assert!(language_for("rust").is_some());
    }

    #[test]
    fn fence_info_strings_still_name_their_language() {
        // ```rust,ignore and ```js {1,3} are both common in the wild.
        assert!(highlight("rust,ignore", "let x = 1;").is_some());
        assert!(highlight("js {1,3}", "const x = 1;").is_some());
        assert!(
            highlight("RUST", "let x = 1;").is_some(),
            "case-insensitive"
        );
    }

    #[test]
    fn every_required_language_is_supported() {
        for lang in [
            "rust",
            "python",
            "javascript",
            "typescript",
            "json",
            "bash",
            "sh",
            "toml",
            "yaml",
            "markdown",
            "diff",
        ] {
            assert!(language_for(lang).is_some(), "{lang} is a D92 minimum");
        }
    }

    #[test]
    fn text_survives_highlighting_unchanged() {
        let src = "fn main() {\n    println!(\"hello, 世界\");\n}";
        assert_eq!(rejoin("rust", src), src);
        let json = "{\n  \"a\": [1, true, null]\n}";
        assert_eq!(rejoin("json", json), json);
    }

    /// A code block arriving a chunk at a time must not panic, and the settled
    /// result must equal the one-shot result — otherwise the live tail and the
    /// scrollback copy of the same block would disagree.
    #[test]
    fn streaming_prefixes_converge_on_the_one_shot_result() {
        let src = "/* a\n   multiline comment */\nfn main() {\n    let s = \"x\";\n}";
        for end in 1..=src.len() {
            if !src.is_char_boundary(end) {
                continue;
            }
            // Must not panic on any prefix, including one that cuts a token in
            // half or leaves a comment unterminated.
            let _ = highlight("rust", &src[..end]);
        }
        assert_eq!(classes("rust", src), classes("rust", src), "deterministic");
        assert_eq!(rejoin("rust", src), src, "settled text is intact");
    }

    #[test]
    fn memoized_result_is_shared_not_recomputed() {
        let src = "let memo = 1;\n";
        let first = highlight("rust", src).expect("rust");
        let second = highlight("rust", src).expect("rust");
        assert!(Rc::ptr_eq(&first, &second), "second call hits the memo");
    }

    #[test]
    fn memo_is_bounded() {
        for i in 0..(MEMO_CAPACITY + 32) {
            let _ = highlight("rust", &format!("let x{i} = {i};"));
        }
        let len = MEMO.with(|m| m.borrow().0.len());
        assert!(
            len <= MEMO_CAPACITY,
            "memo grew to {len}, cap is {MEMO_CAPACITY}"
        );
    }

    #[test]
    fn oversized_blocks_are_left_alone() {
        let huge = "x\n".repeat(MAX_HIGHLIGHT_LINES + 1);
        assert!(highlight("rust", &huge).is_none(), "too many lines");
        let wide = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        assert!(highlight("rust", &wide).is_none(), "too many bytes");
    }

    /// Both palettes must be legible: no class may collide with the code
    /// block's own background, and no two classes may collide with each other.
    #[test]
    fn both_palettes_are_readable_and_distinct() {
        let all = [
            Class::Comment,
            Class::Str,
            Class::Keyword,
            Class::Literal,
            Class::Function,
            Class::Type,
            Class::Meta,
            Class::Operator,
            Class::Plain,
        ];
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for class in all {
                assert_ne!(
                    class.color(&theme),
                    theme.code_block_bg,
                    "{class:?} is invisible on the {name} code background"
                );
            }
            for (i, a) in all.iter().enumerate() {
                for b in &all[i + 1..] {
                    assert_ne!(
                        a.color(&theme),
                        b.color(&theme),
                        "{a:?} and {b:?} are the same colour in {name}"
                    );
                }
            }
        }
    }

    #[test]
    fn tabs_expand_to_the_same_grid_in_both_paths() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("no tabs"), "no tabs");
        // synoptic expands tabs itself; the fallback must agree with it.
        let h = highlight("rust", "\tlet x = 1;").expect("rust");
        let rendered: String = h[0].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(rendered, expand_tabs("\tlet x = 1;"));
    }
}
