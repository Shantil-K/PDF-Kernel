//! Small BM25 keyword index for finding the chunks relevant to a question.
//! No embeddings needed, so it works offline and with any provider.

use std::collections::HashMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "do", "does", "for", "from",
    "had", "has", "have", "how", "in", "is", "it", "its", "of", "on", "or", "that", "the",
    "their", "them", "there", "these", "they", "this", "to", "was", "were", "what", "when",
    "where", "which", "who", "why", "will", "with", "would", "you", "your", "about", "into",
    "than", "then", "so", "if", "not", "no", "we", "our", "i", "me", "my", "he", "she", "his",
    "her", "been", "also", "such", "may", "more", "most", "other", "some", "any", "all",
];

#[derive(Debug, Default)]
pub struct Bm25 {
    postings: HashMap<String, Vec<(usize, u32)>>,
    doc_len: Vec<usize>,
    avg_len: f64,
}

impl Bm25 {
    pub fn build<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut postings: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
        let mut doc_len = Vec::new();
        for (i, text) in texts.into_iter().enumerate() {
            let tokens = tokenize(text);
            doc_len.push(tokens.len());
            let mut tf: HashMap<String, u32> = HashMap::new();
            for t in tokens {
                *tf.entry(t).or_default() += 1;
            }
            for (term, n) in tf {
                postings.entry(term).or_default().push((i, n));
            }
        }
        let avg_len = if doc_len.is_empty() {
            0.0
        } else {
            doc_len.iter().sum::<usize>() as f64 / doc_len.len() as f64
        };
        Self { postings, doc_len, avg_len }
    }

    /// Top `k` (doc index, score) pairs, best first. Empty if nothing matches.
    pub fn search(&self, query: &str, k: usize) -> Vec<(usize, f64)> {
        let n = self.doc_len.len() as f64;
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();

        let mut scores: HashMap<usize, f64> = HashMap::new();
        for term in &terms {
            let Some(list) = self.postings.get(term) else { continue };
            let df = list.len() as f64;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for &(doc, tf) in list {
                let tf = tf as f64;
                let len_norm = 1.0 - B + B * self.doc_len[doc] as f64 / self.avg_len.max(1.0);
                *scores.entry(doc).or_default() += idf * tf * (K1 + 1.0) / (tf + K1 * len_norm);
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(k);
        ranked
    }
}

pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| (w.chars().count() > 1 || w.chars().all(|c| c.is_ascii_digit())) && !STOPWORDS.contains(&w.as_str()))
        .map(stem)
        .collect()
}

/// Very light stemming so "contracts" matches "contract".
fn stem(mut w: String) -> String {
    if w.len() > 4 && w.ends_with("ies") {
        w.truncate(w.len() - 3);
        w.push('y');
    } else if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") {
        w.pop();
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_chunks_first() {
        let docs = [
            "The quarterly revenue grew by 12 percent driven by cloud services.",
            "Employees must submit vacation requests two weeks in advance.",
            "Cloud infrastructure costs rose, but revenue outpaced them.",
        ];
        let idx = Bm25::build(docs);
        let hits = idx.search("How did revenue change?", 3);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(i, _)| *i != 1));
        assert_eq!(idx.search("vacation policies", 1)[0].0, 1);
        assert!(idx.search("the of and", 3).is_empty());
    }

    #[test]
    fn stems_plurals() {
        assert_eq!(tokenize("Contracts policies class"), ["contract", "policy", "class"]);
    }
}
