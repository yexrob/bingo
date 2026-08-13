//! Zero-dependency BM25 scorer for the small corpora bingo recalls from
//! (experience entries, memory facts): built per query, in memory, no index
//! files — dozens of documents score in microseconds, so a search engine
//! dependency would buy nothing (D75).
//!
//! Tokenization carries the recall semantics:
//! - ASCII words are prefix-stemmed to 4 chars, the same ≥4-char-prefix rule
//!   the old experience matcher used ("migrate" finds "migration").
//! - CJK runs become character bigrams — without this a Chinese sentence is
//!   one giant token and nothing ever matches.

use std::collections::HashMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Word tokens shorter than this are noise ("a", "x"); CJK singles stay.
const MIN_WORD_CHARS: usize = 2;
/// ASCII words are cut to this many chars, collapsing inflections.
const STEM_CHARS: usize = 4;

/// Ranked hits below this fraction of the best score are stopword riders, not
/// matches.
const RELATIVE_FLOOR: f64 = 0.25;

/// One document: token → field-weighted term frequency. Build with `field`,
/// weighting the fields that carry the most meaning (a trigger keyword says
/// more than a step line).
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

/// A scored corpus. `N` and document frequencies are computed once at build.
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

    /// One score per document, in corpus order; 0.0 = no query token matched.
    ///
    /// idf is the Lucene form `ln(1 + (N - df + 0.5)/(df + 0.5))` — always
    /// positive, so a token every document shares still matches (these corpora
    /// are small enough that "everywhere" often just means three entries),
    /// while contributing little next to a rare one.
    pub fn score(&self, query: &str) -> Vec<f64> {
        let n = self.docs.len() as f64;
        let tokens = tokenize(query);
        self.docs
            .iter()
            .map(|doc| {
                tokens
                    .iter()
                    .map(|token| {
                        let Some(tf) = doc.tf.get(token) else {
                            return 0.0;
                        };
                        let df = self.df.get(token).copied().unwrap_or(0.0);
                        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                        idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * doc.len / self.avg_len))
                    })
                    .sum()
            })
            .collect()
    }

    /// The top `limit` documents relevant to `query`: score descending, zero
    /// scores dropped, and hits under a quarter of the best dropped too.
    pub fn rank(&self, query: &str, limit: usize) -> Vec<(usize, f64)> {
        let mut hits: Vec<(usize, f64)> = self
            .score(query)
            .into_iter()
            .enumerate()
            .filter(|(_, score)| *score > 0.0)
            .collect();
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let floor = hits.first().map(|(_, best)| best * RELATIVE_FLOOR);
        if let Some(floor) = floor {
            hits.retain(|(_, score)| *score >= floor);
        }
        hits.truncate(limit);
        hits
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

pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, tokens: &mut Vec<String>| {
        if word.chars().count() >= MIN_WORD_CHARS {
            tokens.push(word.chars().take(STEM_CHARS).collect());
        }
        word.clear();
    };
    let flush_cjk = |run: &mut Vec<char>, tokens: &mut Vec<String>| {
        match run.len() {
            0 => {}
            // A lone ideograph has no bigram; it is still a word.
            1 => tokens.push(run[0].to_string()),
            _ => tokens.extend(run.windows(2).map(|pair| pair.iter().collect::<String>())),
        }
        run.clear();
    };
    for c in text.chars().flat_map(char::to_lowercase) {
        if is_cjk(c) {
            flush_word(&mut word, &mut tokens);
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk_run, &mut tokens);
            word.push(c);
        } else {
            flush_word(&mut word, &mut tokens);
            flush_cjk(&mut cjk_run, &mut tokens);
        }
    }
    flush_word(&mut word, &mut tokens);
    flush_cjk(&mut cjk_run, &mut tokens);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_stems_ascii_and_bigrams_cjk() {
        assert_eq!(tokenize("Migrate the DB"), vec!["migr", "the", "db"]);
        assert_eq!(
            tokenize("migration migrate"),
            vec!["migr", "migr"],
            "inflections collapse to the same stem"
        );
        assert_eq!(tokenize("上下文压缩"), vec!["上下", "下文", "文压", "压缩"]);
        assert_eq!(
            tokenize("压 x"),
            vec!["压"],
            "a lone ideograph survives, a lone letter is noise"
        );
        assert_eq!(
            tokenize("fix 压缩 bug"),
            vec!["fix", "压缩", "bug"],
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
    fn score_finds_the_right_document_across_scripts() {
        let bm25 = corpus();
        let scores = bm25.score("run the migration now");
        assert!(scores[0] > 0.0);
        assert_eq!(scores[1], 0.0);
        assert_eq!(scores[2], 0.0);

        let scores = bm25.score("怎么做上下文压缩");
        assert!(scores[2] > 0.0, "CJK bigrams overlap: {scores:?}");
        assert_eq!(scores[0], 0.0);

        assert!(
            bm25.score("nothing relevant here")
                .iter()
                .all(|s| *s == 0.0),
            "no shared token, no score"
        );
    }

    #[test]
    fn rank_orders_drops_zeroes_and_applies_the_floor() {
        let bm25 = Bm25::new(vec![
            Document::default()
                .field("compact compact compact projection", 1.0)
                .field("compaction cache", 2.0),
            Document::default().field("compact once in passing among many other words", 1.0),
            Document::default().field("wholly unrelated entry", 1.0),
        ]);
        let ranked = bm25.rank("compact the context", 10);
        assert_eq!(ranked.first().map(|(i, _)| *i), Some(0));
        assert!(
            ranked.iter().all(|(i, _)| *i != 2),
            "zero-score documents never rank"
        );
        for (_, score) in &ranked {
            assert!(*score >= ranked[0].1 * RELATIVE_FLOOR);
        }
        assert!(
            bm25.rank("", 10).is_empty(),
            "an empty query recalls nothing"
        );
        assert!(
            Bm25::new(Vec::new()).rank("compact", 10).is_empty(),
            "an empty corpus recalls nothing"
        );
    }

    /// The shared-everywhere token still matches (Lucene idf stays positive):
    /// three entries all triggered by "migration" must all be findable.
    #[test]
    fn ubiquitous_tokens_still_match_in_tiny_corpora() {
        let bm25 = Bm25::new(vec![
            Document::default().field("migration", 1.0),
            Document::default().field("migration", 1.0),
            Document::default().field("migration", 1.0),
        ]);
        assert!(bm25.score("migration").iter().all(|s| *s > 0.0));
    }
}
