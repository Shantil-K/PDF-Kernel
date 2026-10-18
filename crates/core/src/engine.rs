use crate::chunk::{chunk_pages, Chunk};
use crate::config::{Config, ProviderConfig};
use crate::error::{Error, Result};
use crate::provider::{ChatMessage, ChatOptions, Completion, ProviderStatus, Router};
use crate::search::Bm25;
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use tokio::sync::Semaphore;

/// Notes produced per section when summarizing long documents.
const SECTION_NOTE_TOKENS: u32 = 900;
const HISTORY_TURNS: usize = 6;

pub struct Document {
    pub id: String,
    pub name: String,
    pages: Vec<String>,
    chunks: Vec<Chunk>,
    index: Bm25,
    chars: usize,
}

impl Document {
    pub fn info(&self) -> DocumentInfo {
        DocumentInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            pages: self.pages.len(),
            chars: self.chars,
            chunks: self.chunks.len(),
        }
    }

    pub fn pages(&self) -> &[String] {
        &self.pages
    }

    /// Whole text with `[Page N]` markers so the model can cite pages.
    fn marked_text(&self) -> String {
        self.pages
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.trim().is_empty())
            .map(|(i, p)| format!("[Page {}]\n{}", i + 1, p.trim()))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentInfo {
    pub id: String,
    pub name: String,
    pub pages: usize,
    pub chars: usize,
    pub chunks: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SummaryStyle {
    Brief,
    #[default]
    Detailed,
    Bullets,
}

impl FromStr for SummaryStyle {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "brief" | "short" => Ok(Self::Brief),
            "detailed" | "long" => Ok(Self::Detailed),
            "bullets" | "bullet" | "points" => Ok(Self::Bullets),
            _ => Err(format!("unknown style '{s}' (use brief, detailed or bullets)")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub document_id: String,
    pub style: SummaryStyle,
    pub text: String,
    pub provider: String,
    pub model: String,
    /// How many sections the document was split into (1 = sent whole).
    pub sections: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AskOptions {
    #[serde(default)]
    pub provider: Option<String>,
    /// Earlier turns of the conversation, oldest first.
    #[serde(default)]
    pub history: Vec<ChatMessage>,
    /// How many chunks to retrieve for long documents.
    #[serde(default)]
    pub top_k: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub page_start: usize,
    pub page_end: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub document_id: String,
    pub text: String,
    /// True when the whole document fit in context, so no excerpts were picked.
    pub used_full_document: bool,
    pub sources: Vec<Source>,
    pub provider: String,
    pub model: String,
}

struct Section {
    page_start: usize,
    page_end: usize,
    text: String,
}

/// The engine. Cheap to share behind an `Arc`; every method takes `&self`.
pub struct Engine {
    cfg: Config,
    router: RwLock<Arc<Router>>,
    docs: RwLock<HashMap<String, Arc<Document>>>,
}

impl Engine {
    pub fn new(mut cfg: Config) -> Result<Self> {
        cfg.normalize();
        let router = Router::new(&cfg)?;
        Ok(Self { cfg, router: RwLock::new(Arc::new(router)), docs: RwLock::new(HashMap::new()) })
    }

    pub fn from_env() -> Result<Self> {
        Self::new(Config::from_env())
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn providers(&self) -> Vec<ProviderStatus> {
        self.router().status()
    }

    /// Replace the provider list (for example after the user edits their API
    /// keys). Loaded documents are kept; requests already running finish on
    /// the old providers.
    pub fn set_providers(&self, providers: Vec<ProviderConfig>) -> Result<()> {
        let cfg = Config { providers, ..self.cfg.clone() };
        let router = Router::new(&cfg)?;
        *self.router.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(router);
        Ok(())
    }

    fn router(&self) -> Arc<Router> {
        self.router.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn load_pdf_file(&self, path: impl AsRef<Path>) -> Result<DocumentInfo> {
        let path = path.as_ref();
        let bytes = tokio::fs::read(path).await?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document.pdf".into());
        self.load_pdf_bytes(name, bytes).await
    }

    pub async fn load_pdf_bytes(&self, name: impl Into<String>, bytes: Vec<u8>) -> Result<DocumentInfo> {
        if bytes.len() > self.cfg.max_pdf_bytes {
            return Err(Error::InvalidRequest(format!(
                "PDF is larger than the {} MB limit",
                self.cfg.max_pdf_bytes / (1024 * 1024)
            )));
        }
        let pages = tokio::task::spawn_blocking(move || crate::pdf::extract_pages(&bytes))
            .await
            .map_err(|e| Error::Internal(e.to_string()))??;
        self.add_pages(name, pages)
    }

    /// Add a document from already-extracted page text. Useful for other
    /// formats or for front-ends that do their own extraction.
    pub fn add_pages(&self, name: impl Into<String>, pages: Vec<String>) -> Result<DocumentInfo> {
        let chars: usize = pages.iter().map(|p| p.trim().len()).sum();
        if chars == 0 {
            return Err(Error::EmptyDocument);
        }
        let chunks = chunk_pages(&pages, self.cfg.chunk_chars, self.cfg.chunk_overlap);
        let index = Bm25::build(chunks.iter().map(|c| c.text.as_str()));
        let doc = Document {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: name.into(),
            pages,
            chunks,
            index,
            chars,
        };
        let info = doc.info();
        self.docs.write().unwrap_or_else(|e| e.into_inner()).insert(doc.id.clone(), Arc::new(doc));
        Ok(info)
    }

    pub fn list_documents(&self) -> Vec<DocumentInfo> {
        let mut list: Vec<_> = self
            .docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|d| d.info())
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        list
    }

    pub fn document(&self, id: &str) -> Result<Arc<Document>> {
        self.docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| Error::DocumentNotFound(id.to_string()))
    }

    pub fn remove_document(&self, id: &str) -> bool {
        self.docs.write().unwrap_or_else(|e| e.into_inner()).remove(id).is_some()
    }

    /// Summarize a document. Short documents go in one request; long ones are
    /// summarized section by section (in parallel), then combined.
    pub async fn summarize(&self, id: &str, style: SummaryStyle, provider: Option<String>) -> Result<Summary> {
        let doc = self.document(id)?;
        let budget = self.cfg.max_context_chars;
        let opts = ChatOptions { provider, temperature: 0.2, max_tokens: None };

        let full = doc.marked_text();
        let (source_text, sections) = if full.len() <= budget {
            (full, 1)
        } else {
            let sections = group_pages(&doc.pages, budget);
            let n = sections.len();
            let note_opts = ChatOptions { max_tokens: Some(SECTION_NOTE_TOKENS), ..opts.clone() };
            let mut notes = self
                .run_parallel(sections.iter().enumerate().map(|(i, s)| prompts::section_notes(&doc.name, i + 1, n, s)), &note_opts)
                .await?;

            // Combine notes until they fit in one request.
            for _ in 0..4 {
                if notes.len() <= 1 || joined_len(&notes) <= budget {
                    break;
                }
                let batches = batch_texts(&notes, budget);
                notes = self.run_parallel(batches.iter().map(|b| prompts::combine_notes(&doc.name, b)), &note_opts).await?;
            }
            let mut combined = notes.join("\n\n---\n\n");
            truncate_at_char(&mut combined, budget);
            (combined, n)
        };

        let c = self
            .router()
            .chat(&prompts::final_summary(&doc.name, style, &source_text, sections > 1), &opts)
            .await?;
        Ok(Summary {
            document_id: doc.id.clone(),
            style,
            text: c.text,
            provider: c.provider,
            model: c.model,
            sections,
        })
    }

    /// Answer a question about a document, citing pages.
    pub async fn ask(&self, id: &str, question: &str, opts: AskOptions) -> Result<Answer> {
        let question = question.trim();
        if question.is_empty() {
            return Err(Error::InvalidRequest("question is empty".into()));
        }
        let doc = self.document(id)?;
        let budget = self.cfg.max_context_chars;

        let full = doc.marked_text();
        let used_full_document = full.len() <= budget;
        let (context, sources) = if used_full_document {
            (full, Vec::new())
        } else {
            let k = opts.top_k.unwrap_or(8).clamp(1, 50);
            let mut hits: Vec<usize> = doc.index.search(question, k).into_iter().map(|(i, _)| i).collect();
            if hits.is_empty() {
                hits = (0..doc.chunks.len().min(k)).collect();
            }
            let mut picked = Vec::new();
            let mut used = 0;
            for i in hits {
                let len = doc.chunks[i].text.len() + 24;
                if used + len <= budget {
                    used += len;
                    picked.push(i);
                }
            }
            picked.sort_unstable();
            let context = picked
                .iter()
                .map(|&i| {
                    let c = &doc.chunks[i];
                    format!("[{}]\n{}", page_label(c.page_start, c.page_end), c.text)
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let sources = picked
                .iter()
                .map(|&i| {
                    let c = &doc.chunks[i];
                    Source { page_start: c.page_start, page_end: c.page_end, excerpt: excerpt(&c.text, 220) }
                })
                .collect();
            (context, sources)
        };

        let mut history: Vec<ChatMessage> = opts
            .history
            .into_iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .collect();
        history.drain(..history.len().saturating_sub(HISTORY_TURNS));
        let messages = prompts::question(&doc.name, &context, question, used_full_document, history);
        let chat_opts = ChatOptions { provider: opts.provider, temperature: 0.1, max_tokens: None };
        let c: Completion = self.router().chat(&messages, &chat_opts).await?;
        Ok(Answer {
            document_id: doc.id.clone(),
            text: c.text,
            used_full_document,
            sources,
            provider: c.provider,
            model: c.model,
        })
    }

    async fn run_parallel(
        &self,
        prompts: impl Iterator<Item = Vec<ChatMessage>>,
        opts: &ChatOptions,
    ) -> Result<Vec<String>> {
        let sem = Arc::new(Semaphore::new(self.cfg.concurrency));
        let router = self.router();
        let tasks = prompts.map(|messages| {
            let sem = sem.clone();
            let router = router.clone();
            async move {
                let _permit = sem.acquire_owned().await.map_err(|e| Error::Internal(e.to_string()))?;
                router.chat(&messages, opts).await.map(|c| c.text)
            }
        });
        try_join_all(tasks).await
    }
}

fn page_label(start: usize, end: usize) -> String {
    if start == end {
        format!("Page {start}")
    } else {
        format!("Pages {start}-{end}")
    }
}

/// Group whole pages into sections that fit the budget; split oversized pages.
fn group_pages(pages: &[String], budget: usize) -> Vec<Section> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut start, mut end) = (0, 0);
    for (i, page) in pages.iter().enumerate() {
        let no = i + 1;
        let page = page.trim();
        if page.is_empty() {
            continue;
        }
        let block = format!("[Page {no}]\n{page}\n\n");
        if !cur.is_empty() && cur.len() + block.len() > budget {
            out.push(Section { page_start: start, page_end: end, text: std::mem::take(&mut cur) });
        }
        if block.len() > budget {
            for piece in split_by_chars(&block, budget) {
                out.push(Section { page_start: no, page_end: no, text: piece });
            }
            continue;
        }
        if cur.is_empty() {
            start = no;
        }
        end = no;
        cur.push_str(&block);
    }
    if !cur.is_empty() {
        out.push(Section { page_start: start, page_end: end, text: cur });
    }
    out
}

fn split_by_chars(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if cur.len() + ch.len_utf8() > max {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Pack texts into batches under the budget. Each batch takes at least two
/// items when possible so every round shrinks the list.
fn batch_texts(texts: &[String], budget: usize) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut len = 0;
    for t in texts {
        if cur.len() >= 2 && len + t.len() > budget {
            out.push(std::mem::take(&mut cur));
            len = 0;
        }
        len += t.len();
        cur.push(t.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn joined_len(texts: &[String]) -> usize {
    texts.iter().map(|t| t.len() + 7).sum()
}

fn truncate_at_char(s: &mut String, max: usize) {
    if s.len() > max {
        let mut i = max;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        s.truncate(i);
    }
}

fn excerpt(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>().trim_end())
    }
}

mod prompts {
    use super::{page_label, Section, SummaryStyle};
    use crate::provider::ChatMessage;

    const SUMMARY_SYSTEM: &str = "You summarize documents accurately. Use only the text provided. \
        Never invent facts, numbers or names. Write in the document's language.";

    const QA_SYSTEM: &str = "You answer questions about a document using only the text provided. \
        Cite the pages you used, like (p. 4) or (pp. 4-5). If the answer is not in the text, \
        say you could not find it in the document. Be concise and direct.";

    pub fn section_notes(name: &str, i: usize, n: usize, s: &Section) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system(SUMMARY_SYSTEM),
            ChatMessage::user(format!(
                "This is part {i} of {n} of the document \"{name}\" ({}).\n\
                 Write concise notes on its key points, facts, figures and conclusions. \
                 Keep page references.\n\n{}",
                page_label(s.page_start, s.page_end),
                s.text
            )),
        ]
    }

    pub fn combine_notes(name: &str, notes: &[String]) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system(SUMMARY_SYSTEM),
            ChatMessage::user(format!(
                "These are notes on consecutive parts of \"{name}\". Merge them into one shorter \
                 set of notes, keeping every important point and page reference.\n\n{}",
                notes.join("\n\n---\n\n")
            )),
        ]
    }

    pub fn final_summary(name: &str, style: SummaryStyle, text: &str, from_notes: bool) -> Vec<ChatMessage> {
        let instruction = match style {
            SummaryStyle::Brief => "Write a brief summary: one or two short paragraphs, about 120 words.",
            SummaryStyle::Detailed => {
                "Write a detailed, well-organized summary in Markdown with short section headings. \
                 Cover the purpose, main points, key findings or figures, and conclusions."
            }
            SummaryStyle::Bullets => "Summarize as 6 to 12 Markdown bullet points with the most important facts and takeaways.",
        };
        let source = if from_notes { "notes covering the whole document" } else { "full text" };
        vec![
            ChatMessage::system(SUMMARY_SYSTEM),
            ChatMessage::user(format!("Document: \"{name}\"\nBelow is its {source}.\n{instruction}\n\n{text}")),
        ]
    }

    pub fn question(
        name: &str,
        context: &str,
        question: &str,
        full: bool,
        history: Vec<ChatMessage>,
    ) -> Vec<ChatMessage> {
        let what = if full { "full text" } else { "most relevant excerpts" };
        let mut messages = vec![ChatMessage::system(QA_SYSTEM)];
        messages.extend(history);
        messages.push(ChatMessage::user(format!(
            "Document: \"{name}\"\nHere is its {what}:\n\n{context}\n\nQuestion: {question}"
        )));
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        Engine::new(Config::default()).unwrap()
    }

    #[test]
    fn add_list_remove() {
        let e = engine();
        let info = e.add_pages("a.pdf", vec!["hello world".into(), "page two".into()]).unwrap();
        assert_eq!(info.pages, 2);
        assert_eq!(e.list_documents().len(), 1);
        assert!(e.remove_document(&info.id));
        assert!(matches!(e.document(&info.id), Err(Error::DocumentNotFound(_))));
    }

    #[test]
    fn rejects_empty_document() {
        assert!(matches!(engine().add_pages("x", vec!["  ".into()]), Err(Error::EmptyDocument)));
    }

    #[tokio::test]
    async fn no_provider_error() {
        let e = engine();
        let info = e.add_pages("a.pdf", vec!["some text".into()]).unwrap();
        let err = e.summarize(&info.id, SummaryStyle::Brief, None).await.unwrap_err();
        assert!(matches!(err, Error::NoProvider));
    }

    #[test]
    fn groups_pages_within_budget() {
        let pages: Vec<String> = (0..10).map(|_| "word ".repeat(200)).collect();
        let sections = group_pages(&pages, 2_500);
        assert!(sections.len() >= 5);
        assert!(sections.iter().all(|s| s.text.len() <= 2_500));
        assert_eq!(sections[0].page_start, 1);
        assert_eq!(sections.last().unwrap().page_end, 10);
    }

    #[test]
    fn batches_shrink_list() {
        let notes: Vec<String> = (0..7).map(|_| "x".repeat(900)).collect();
        let batches = batch_texts(&notes, 1_000);
        assert!(batches.len() < notes.len());
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), 7);
    }
}
