use serde::Serialize;

/// A slice of the document used for retrieval, tagged with its page range.
#[derive(Debug, Clone, Serialize)]
pub struct Chunk {
    pub index: usize,
    pub page_start: usize,
    pub page_end: usize,
    pub text: String,
}

/// Split pages into overlapping chunks of about `size` bytes, preferring to
/// break on whitespace. Pages are 1-based in the output.
pub fn chunk_pages(pages: &[String], size: usize, overlap: usize) -> Vec<Chunk> {
    let size = size.max(50);
    let overlap = overlap.min(size / 2);

    let mut full = String::new();
    let mut page_starts = Vec::with_capacity(pages.len());
    for p in pages {
        page_starts.push(full.len());
        full.push_str(p.trim());
        full.push_str("\n\n");
    }
    let page_at = |offset: usize| match page_starts.binary_search(&offset) {
        Ok(i) => i + 1,
        Err(i) => i, // i >= 1 because page_starts[0] == 0
    };

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < full.len() {
        let mut end = floor_boundary(&full, (start + size).min(full.len()));
        if end < full.len() {
            if let Some(ws) = full[start..end].rfind(char::is_whitespace) {
                if ws > size / 2 {
                    end = start + ws;
                }
            }
        }
        if end <= start {
            end = ceil_boundary(&full, start + 1);
        }
        let text = full[start..end].trim();
        if !text.is_empty() {
            chunks.push(Chunk {
                index: chunks.len(),
                page_start: page_at(start),
                page_end: page_at(end - 1).max(page_at(start)),
                text: text.to_string(),
            });
        }
        if end >= full.len() {
            break;
        }
        let mut next = ceil_boundary(&full, end.saturating_sub(overlap).max(start + 1));
        // Start the overlap on a word boundary.
        if let Some(ws) = full[next..end].find(char::is_whitespace) {
            next += ws;
        }
        start = next;
    }
    chunks
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_all_text_with_page_ranges() {
        let pages: Vec<String> = (1..=5).map(|i| format!("page {i} ").repeat(60)).collect();
        let chunks = chunk_pages(&pages, 300, 50);
        assert!(chunks.len() > 5);
        assert_eq!(chunks[0].page_start, 1);
        assert_eq!(chunks.last().unwrap().page_end, 5);
        for c in &chunks {
            assert!(c.text.len() <= 300);
            assert!(c.page_start <= c.page_end);
        }
        for p in 1..=5 {
            assert!(chunks.iter().any(|c| c.text.contains(&format!("page {p}"))));
        }
    }

    #[test]
    fn handles_multibyte_and_unbroken_text() {
        let pages = vec!["é".repeat(500), "x".repeat(700)];
        let chunks = chunk_pages(&pages, 120, 30);
        assert!(!chunks.is_empty());
        assert_eq!(chunks.last().unwrap().page_end, 2);
    }

    #[test]
    fn empty_input() {
        assert!(chunk_pages(&[], 100, 10).is_empty());
        assert!(chunk_pages(&["  ".into()], 100, 10).is_empty());
    }
}
