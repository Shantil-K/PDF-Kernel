use crate::error::{Error, Result};

/// Extract cleaned text for each page. Index 0 is page 1.
pub fn extract_pages(bytes: &[u8]) -> Result<Vec<String>> {
    // Some PDFs have junk before the header, so look a little way in.
    let head = &bytes[..bytes.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(Error::Pdf("file is not a PDF".into()));
    }
    let pages = match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes)) {
        Ok(Ok(pages)) => pages,
        Ok(Err(e)) => return Err(Error::Pdf(e.to_string())),
        Err(_) => {
            return Err(Error::Pdf(
                "the PDF parser could not handle this file (it may be malformed or encrypted)".into(),
            ))
        }
    };
    Ok(pages.iter().map(|p| clean(p)).collect())
}

/// Tidy extracted text: collapse spacing, keep paragraph breaks, and re-join
/// words hyphenated across line ends.
pub fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut paragraph_break = false;
    for line in raw.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            paragraph_break = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            if paragraph_break {
                out.push_str("\n\n");
            } else if out.ends_with('-') && line.starts_with(|c: char| c.is_lowercase()) {
                out.pop();
            } else {
                out.push('\n');
            }
        }
        paragraph_break = false;
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_text() {
        let raw = "  Hello   world\nthis is a hyph-\nenated word\n\n\n\nNew   para ";
        assert_eq!(clean(raw), "Hello world\nthis is a hyphenated word\n\nNew para");
    }

    #[test]
    fn rejects_non_pdf() {
        assert!(matches!(extract_pages(b"hello"), Err(Error::Pdf(_))));
    }
}
