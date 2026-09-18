use std::collections::HashMap;

pub const DEFAULT_K1: f64 = 1.5;
pub const DEFAULT_B: f64 = 0.75;

/// Latin alphanumeric runs (lowercased) plus CJK unigrams and bigrams.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut latin = String::new();
    let mut prev_cjk: Option<char> = None;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            prev_cjk = None;
            latin.push(c.to_ascii_lowercase());
        } else {
            if !latin.is_empty() {
                tokens.push(std::mem::take(&mut latin));
            }
            if is_cjk(c) {
                if let Some(prev) = prev_cjk {
                    let mut bigram = String::with_capacity(6);
                    bigram.push(prev);
                    bigram.push(c);
                    tokens.push(bigram);
                }
                tokens.push(c.to_string());
                prev_cjk = Some(c);
            } else {
                prev_cjk = None;
            }
        }
    }
    if !latin.is_empty() {
        tokens.push(latin);
    }
    tokens
}

fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x3040..=0x30FF | 0xAC00..=0xD7AF
    )
}

#[derive(Debug, Default, Clone)]
pub struct Bm25Index {
    doc_tfs: Vec<HashMap<String, u32>>,
    doc_lens: Vec<usize>,
    doc_freqs: HashMap<String, usize>,
    avg_len: f64,
}

impl Bm25Index {
    pub fn build(docs: &[Vec<String>]) -> Self {
        let mut doc_tfs = Vec::with_capacity(docs.len());
        let mut doc_lens = Vec::with_capacity(docs.len());
        let mut doc_freqs: HashMap<String, usize> = HashMap::new();
        let mut total = 0usize;
        for doc in docs {
            let mut tf: HashMap<String, u32> = HashMap::new();
            for token in doc {
                *tf.entry(token.clone()).or_insert(0) += 1;
            }
            for term in tf.keys() {
                *doc_freqs.entry(term.clone()).or_insert(0) += 1;
            }
            doc_lens.push(doc.len());
            total += doc.len();
            doc_tfs.push(tf);
        }
        let avg_len = if docs.is_empty() {
            0.0
        } else {
            total as f64 / docs.len() as f64
        };
        Self {
            doc_tfs,
            doc_lens,
            doc_freqs,
            avg_len,
        }
    }

    pub fn len(&self) -> usize {
        self.doc_tfs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.doc_tfs.is_empty()
    }

    /// Scored docs, highest first, ties broken by doc id. Only score > 0.
    pub fn search(&self, query: &[String], limit: usize) -> Vec<(usize, f64)> {
        self.score(query, DEFAULT_K1, DEFAULT_B, limit)
    }

    pub fn score(&self, query: &[String], k1: f64, b: f64, limit: usize) -> Vec<(usize, f64)> {
        let n = self.doc_tfs.len() as f64;
        let mut scores = Vec::new();
        for (i, tf) in self.doc_tfs.iter().enumerate() {
            let mut score = 0.0;
            for term in query {
                let Some(&df) = self.doc_freqs.get(term) else {
                    continue;
                };
                let f = f64::from(*tf.get(term).unwrap_or(&0));
                if f == 0.0 {
                    continue;
                }
                let idf = ((n - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
                let norm = if self.avg_len > 0.0 {
                    1.0 - b + b * self.doc_lens[i] as f64 / self.avg_len
                } else {
                    1.0
                };
                score += idf * (f * (k1 + 1.0)) / (f + k1 * norm);
            }
            if score > 0.0 {
                scores.push((i, score));
            }
        }
        scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        scores.truncate(limit);
        scores
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_mixed_chinese_english() {
        let tokens = tokenize("Rust语言 Memory");
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"memory".to_string()));
        assert!(tokens.contains(&"语".to_string()));
        assert!(tokens.contains(&"言".to_string()));
        assert!(tokens.contains(&"语言".to_string()));
    }

    #[test]
    fn single_cjk_char_is_kept_as_unigram() {
        let tokens = tokenize("配置");
        assert!(tokens.contains(&"配".to_string()));
        assert!(tokens.contains(&"置".to_string()));
        assert!(tokens.contains(&"配置".to_string()));
        let lone = tokenize("a 锈 b");
        assert!(lone.contains(&"锈".to_string()));
    }

    #[test]
    fn latin_runs_lowercase_and_split_on_punctuation() {
        let tokens = tokenize("Hello, World!GPT-4o");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"gpt".to_string()));
        assert!(tokens.contains(&"4o".to_string()));
    }

    #[test]
    fn ranks_relevant_documents_first() {
        let docs = vec![
            tokenize("rust 系统编程 性能 内存安全"),
            tokenize("香蕉 苹果 水果 沙拉 食谱"),
            tokenize("rust borrow checker 所有权 生命周期"),
        ];
        let index = Bm25Index::build(&docs);
        let hits = index.search(&tokenize("所有权"), 10);
        assert_eq!(hits[0].0, 2);
        let hits = index.search(&tokenize("水果"), 10);
        assert_eq!(hits[0].0, 1);
        let hits = index.search(&tokenize("rust"), 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(_, s)| *s > 0.0));
    }

    #[test]
    fn repeated_term_scores_higher_than_single() {
        let docs = vec![
            tokenize("rust rust rust"),
            tokenize("rust 加上一些别的词让长度差不多"),
        ];
        let index = Bm25Index::build(&docs);
        let hits = index.search(&tokenize("rust"), 10);
        assert_eq!(hits[0].0, 0);
    }

    #[test]
    fn no_match_returns_empty() {
        let docs = vec![tokenize("香蕉 苹果")];
        let index = Bm25Index::build(&docs);
        assert!(index.search(&tokenize("rust"), 10).is_empty());
    }
}
