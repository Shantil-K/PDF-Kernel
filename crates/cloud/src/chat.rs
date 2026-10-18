//! Chat history format. Entries are never edited, only added, and each has a
//! unique id, so two devices' copies merge without losing anything.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const CHAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatEntry {
    pub id: String,
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
    /// Milliseconds since the Unix epoch.
    pub created_at: i64,
    /// `message` (default) or `summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Extra data from the engine, e.g. cited sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl ChatEntry {
    pub fn is_summary(&self) -> bool {
        self.kind.as_deref() == Some("summary")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChatLog {
    pub version: u32,
    pub doc_id: String,
    pub messages: Vec<ChatEntry>,
}

impl ChatLog {
    pub fn new(doc_id: impl Into<String>) -> Self {
        Self { version: CHAT_VERSION, doc_id: doc_id.into(), messages: Vec::new() }
    }

    /// Add entries not already present, keeping chronological order.
    /// Returns how many were new.
    pub fn merge(&mut self, entries: impl IntoIterator<Item = ChatEntry>) -> usize {
        let mut seen: HashSet<String> = self.messages.iter().map(|m| m.id.clone()).collect();
        let before = self.messages.len();
        for e in entries {
            if seen.insert(e.id.clone()) {
                self.messages.push(e);
            }
        }
        self.messages.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        self.messages.len() - before
    }

    pub fn contains_all(&self, ids: &[String]) -> bool {
        let have: HashSet<&str> = self.messages.iter().map(|m| m.id.as_str()).collect();
        ids.iter().all(|id| have.contains(id.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, t: i64) -> ChatEntry {
        ChatEntry {
            id: id.into(),
            role: "user".into(),
            content: id.into(),
            created_at: t,
            kind: None,
            device: None,
            provider: None,
            model: None,
            meta: None,
        }
    }

    #[test]
    fn merge_is_union_in_time_order() {
        // Two devices added messages to the same chat while apart.
        let mut a = ChatLog::new("d");
        a.merge([e("a1", 1), e("a2", 3)]);
        let mut b = ChatLog::new("d");
        b.merge([e("a1", 1), e("b1", 2)]);

        let mut ab = a.clone();
        assert_eq!(ab.merge(b.messages.clone()), 1);
        let mut ba = b.clone();
        ba.merge(a.messages.clone());
        assert_eq!(ab, ba);
        let ids: Vec<_> = ab.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a1", "b1", "a2"]);
        assert_eq!(ab.merge(a.messages.clone()), 0);
        assert!(ab.contains_all(&["b1".into(), "a2".into()]));
    }
}
