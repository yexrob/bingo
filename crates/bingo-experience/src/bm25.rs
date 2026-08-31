//! The one ranking path (ADR-0014 §5): a zero-dependency BM25 over the dozens
//! of entries a project accumulates, built per query, in memory, no index file
//! to drift.
//!
//! Tokenization carries the semantics:
//! - ASCII words are cut to a 4-char prefix, so "migrate" finds "migration";
//!   anything shorter than two characters is noise.
//! - CJK runs become character bigrams — without this a Chinese sentence is
//!   one token and nothing ever matches.

use std::collections::HashMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Word tokens shorter than this are noise ("a", "x"); CJK singles stay.
const MIN_WORD_CHARS: usize = 2;
/// ASCII words are cut to this many chars, collapsing inflections.
const STEM_CHARS: usize = 4;

/// Hits below this fraction of the best are stopword riders, not matches.
const RELATIVE_FLOOR: f64 = 0.25;

/// One document: token → field-weighted term frequency. Build it with `field`,
/// weighting what carries the most meaning (a trigger word says more than a
/// step line).
#[derive(Debug, Default, Clone)]
pub struct Document {
    tf: HashMap<String, f64>,
    len: f64,
}

impl Document {
    pub fn field(mut self, text: &str, weight: f64) -> Self {
        for token in tokenize(text) {
            *self.tf.entry(token).or_default() += weight;
            self.len += weight;
        }
        self
    }
}

/// A scored corpus. `N` and the document frequencies are computed once.
#[derive(Debug)]
pub struct Bm25 {
    docs: Vec<Document>,
    df: HashMap<String, f64>,
    avg_len: f64,
}

impl Bm25 {
    pub fn new(docs: Vec<Document>) -> Self {
        let mut df: HashMap<String, f64> = HashMap::new();
        for doc in &docs {
            for token in doc.tf.keys() {
                *df.entry(token.clone()).or_default() += 1.0;
            }
        }
        let total: f64 = docs.iter().map(|d| d.len).sum();
        let avg_len = if docs.is_empty() {
            0.0
        } else {
            (total / docs.len() as f64).max(1.0)
        };
        Self { docs, df, avg_len }
    }

    /// The documents `query` matches, best first, as `(index, score)`.
    ///
    /// Zero scores are never hits. With `floor`, anything under a quarter of
    /// the best goes too: recall is unsolicited and wants the guard, a search
    /// a person or the model asked for wants the best answer even when it is
    /// weak. Ties keep corpus order, which is the caller's ordering — there is
    /// no second sort chain to disagree with it.
    pub fn rank(&self, query: &str, floor: bool) -> Vec<(usize, f64)> {
        let mut hits: Vec<(usize, f64)> = self
            .score(query)
            .into_iter()
            .enumerate()
            .filter(|(_, score)| *score > 0.0)
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        if floor && let Some((_, best)) = hits.first().copied() {
            hits.retain(|(_, score)| *score >= best * RELATIVE_FLOOR);
        }
        hits
    }

    /// One score per document, in corpus order; 0.0 = no query token matched.
    ///
    /// idf is the Lucene form `ln(1 + (N - df + 0.5)/(df + 0.5))` — always
    /// positive, so a token every document shares still matches (three entries
    /// is a corpus where "everywhere" means three), while counting for little
    /// beside a rare one.
    fn score(&self, query: &str) -> Vec<f64> {
        let n = self.docs.len() as f64;
        let tokens = tokenize(query);
        self.docs
            .iter()
            .map(|doc| {
                tokens
                    .iter()
                    .map(|token| self.term(doc, token, n))
                    .sum::<f64>()
            })
            .collect()
    }

    fn term(&self, doc: &Document, token: &str, n: f64) -> f64 {
        let Some(tf) = doc.tf.get(token) else {
            return 0.0;
        };
        let df = self.df.get(token).copied().unwrap_or(0.0);
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * doc.len / self.avg_len))
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}'   // hiragana + katakana
        | '\u{3400}'..='\u{4dbf}' // CJK extension A
        | '\u{4e00}'..='\u{9fff}' // CJK unified ideographs
        | '\u{f900}'..='\u{faff}' // CJK compatibility ideographs
        | '\u{ac00}'..='\u{d7af}' // hangul syllables
    )
}

/// Lowercase, split on anything that is not a letter or a digit, stem ASCII to
/// its prefix, and cut CJK runs into bigrams. Both buffers flush at a script
/// boundary, so `fix 压缩 bug` is three words and not one.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut run: Vec<char> = Vec::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if is_cjk(c) {
            flush_word(&mut word, &mut tokens);
            run.push(c);
        } else if c.is_alphanumeric() {
            flush_run(&mut run, &mut tokens);
            word.push(c);
        } else {
            flush_word(&mut word, &mut tokens);
            flush_run(&mut run, &mut tokens);
        }
    }
    flush_word(&mut word, &mut tokens);
    flush_run(&mut run, &mut tokens);
    tokens
}

fn flush_word(word: &mut String, tokens: &mut Vec<String>) {
    if word.chars().count() >= MIN_WORD_CHARS {
        tokens.push(word.chars().take(STEM_CHARS).collect());
    }
    word.clear();
}

fn flush_run(run: &mut Vec<char>, tokens: &mut Vec<String>) {
    match run.len() {
        0 => {}
        // A lone ideograph has no bigram; it is still a word.
        1 => tokens.push(run[0].to_string()),
        _ => tokens.extend(run.windows(2).map(|pair| pair.iter().collect::<String>())),
    }
    run.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn tokenize_stems_ascii_and_bigrams_cjk() {
        assert_eq!(tokenize("Migrate the DB"), ["migr", "the", "db"]);
        assert_eq!(
            tokenize("migration migrate"),
            ["migr", "migr"],
            "inflections collapse to one stem"
        );
        assert_eq!(tokenize("上下文压缩"), ["上下", "下文", "文压", "压缩"]);
        assert_eq!(
            tokenize("压 x"),
            ["压"],
            "a lone ideograph survives, a lone letter is noise"
        );
        assert_eq!(
            tokenize("fix 压缩 bug"),
            ["fix", "压缩", "bug"],
            "script boundaries split runs"
        );
    }

    fn corpus() -> Bm25 {
        Bm25::new(vec![
            Document::default().field("database migration steps", 1.0),
            Document::default().field("compile a frontend bundle", 1.0),
            Document::default().field("上下文压缩与缓存命中", 1.0),
        ])
    }

    #[test]
    fn ranking_finds_the_right_document_across_scripts() {
        let bm25 = corpus();
        assert_eq!(
            bm25.rank("run the migration now", false)
                .first()
                .map(|(i, _)| *i),
            Some(0)
        );
        assert_eq!(
            bm25.rank("怎么做上下文压缩", false)
                .first()
                .map(|(i, _)| *i),
            Some(2),
            "CJK bigrams overlap"
        );
        assert!(bm25.rank("nothing relevant here", false).is_empty());
        assert!(
            bm25.rank("", false).is_empty(),
            "an empty query recalls nothing"
        );
        assert!(Bm25::new(Vec::new()).rank("migrate", false).is_empty());
    }

    #[test]
    fn the_floor_drops_the_riders_and_no_floor_keeps_them() {
        let bm25 = Bm25::new(vec![
            Document::default()
                .field("compact the context window", 3.0)
                .field("compaction keeps the cache", 1.0),
            // Riders: they share only "the", the word the query could not avoid.
            Document::default().field("the release notes of the day", 1.0),
            Document::default().field("the compiler makes the frontend bundle", 1.0),
            Document::default().field("wholly unrelated entry", 1.0),
        ]);
        let all = bm25.rank("compact the context", false);
        let kept = bm25.rank("compact the context", true);
        assert_eq!(all.first().map(|(i, _)| *i), Some(0));
        assert!(all.iter().all(|(i, _)| *i != 3), "zero scores never rank");
        assert!(kept.len() < all.len(), "the floor dropped nothing: {all:?}");
        for (_, score) in &kept {
            assert!(*score >= all[0].1 * RELATIVE_FLOOR);
        }
    }

    /// The shared-everywhere token still matches: three entries all triggered
    /// by "migration" must all be findable (Lucene idf stays positive).
    #[test]
    fn ubiquitous_tokens_still_match_in_tiny_corpora() {
        let bm25 = Bm25::new(vec![
            Document::default().field("migration", 1.0),
            Document::default().field("migration", 1.0),
            Document::default().field("migration", 1.0),
        ]);
        assert_eq!(bm25.rank("migration", false).len(), 3);
    }

    #[test]
    fn a_weighted_field_outranks_the_same_word_in_a_plain_one() {
        let bm25 = Bm25::new(vec![
            Document::default().field("cache", 1.0),
            Document::default().field("cache", 3.0),
        ]);
        assert_eq!(bm25.rank("cache", false).first().map(|(i, _)| *i), Some(1));
    }

    /// Arbitrary text, with the scripts this tokenizer tells apart.
    fn text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                "[a-zA-Z]{1,12}",
                "[0-9]{1,4}",
                "[\u{4e00}-\u{9fff}]{1,6}",
                "[\u{3040}-\u{30ff}]{1,4}",
                "[ \t.,:;/()\\-]{1,3}",
            ],
            0..12usize,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        /// Whatever the mix of scripts, a token is short, lowercase and never
        /// empty: the corpus is bigrams and 4-char stems and nothing else.
        #[test]
        fn every_token_is_a_stem_or_a_bigram(text in text()) {
            for token in tokenize(&text) {
                let chars: Vec<char> = token.chars().collect();
                prop_assert!(!chars.is_empty());
                prop_assert!(chars.len() <= STEM_CHARS, "{token:?}");
                prop_assert_eq!(token.to_lowercase(), token.clone());
                if chars.iter().any(|c| is_cjk(*c)) {
                    prop_assert!(chars.len() <= 2, "a CJK token is a bigram: {token:?}");
                    prop_assert!(chars.iter().all(|c| is_cjk(*c)), "runs flush at the boundary: {token:?}");
                }
            }
        }

        /// An entry is found by its own words, whatever they are made of.
        #[test]
        fn a_document_is_recalled_by_its_own_text(text in text()) {
            prop_assume!(!tokenize(&text).is_empty());
            let bm25 = Bm25::new(vec![
                Document::default().field("an unrelated english playbook", 1.0),
                Document::default().field(&text, 1.0),
            ]);
            let hits = bm25.rank(&text, false);
            prop_assert!(hits.iter().any(|(i, _)| *i == 1), "{text:?} did not find itself");
            prop_assert!(hits.iter().all(|(_, score)| score.is_finite() && *score > 0.0));
        }

        /// The floor only ever cuts a tail: what it keeps is the head of what
        /// the same query returns without it.
        #[test]
        fn the_floor_keeps_a_prefix_of_the_unfloored_ranking(query in text(), a in text(), b in text()) {
            let bm25 = Bm25::new(vec![
                Document::default().field(&a, 3.0).field(&b, 1.0),
                Document::default().field(&b, 2.0),
                Document::default().field("a third, plainly english, document", 1.0),
            ]);
            let all = bm25.rank(&query, false);
            let kept = bm25.rank(&query, true);
            prop_assert!(kept.len() <= all.len());
            prop_assert_eq!(&kept[..], &all[..kept.len()]);
        }
    }
}
