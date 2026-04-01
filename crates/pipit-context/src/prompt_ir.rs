//! Canonical Prompt IR and Cache-Stable Context Graph (Architecture Task 7)
//!
//! Defines a canonical intermediate representation for prompt construction.
//! Features:
//! - Stable message IDs (content-hash-based, not positional)
//! - Canonical tool schema ordering (sorted by name)
//! - Segment hashes for incremental invalidation
//! - Deterministic prompt assembly for cache stability
//!
//! Prompt construction becomes deterministic: semantically equivalent
//! histories produce identical token footprints.

use pipit_provider::{Message, ToolDeclaration};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ─── Segment Types ──────────────────────────────────────────────────────

/// A segment of the prompt with a stable identity and content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSegment {
    /// Stable ID derived from content (not position).
    pub id: SegmentId,
    /// The segment kind (determines ordering and cacheability).
    pub kind: SegmentKind,
    /// Content hash for incremental invalidation.
    pub content_hash: u64,
    /// Estimated token count.
    pub token_count: u64,
    /// Whether this segment has changed since last prompt build.
    pub dirty: bool,
}

/// Stable segment identifier.
pub type SegmentId = u64;

/// Segment classification for ordering and cache policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SegmentKind {
    /// System prompt — always first, rarely changes.
    SystemPrompt = 0,
    /// Plan/claim context — changes on plan pivots.
    PlanContext = 1,
    /// Conversation summary — changes on compaction.
    Summary = 2,
    /// Repository map — changes on file mutations.
    RepoMap = 3,
    /// Tool schemas — stable once tools are registered.
    ToolSchemas = 4,
    /// Historical messages — stable after compaction.
    History = 5,
    /// Recent messages — changes every turn.
    Recent = 6,
}

// ─── Prompt IR ──────────────────────────────────────────────────────────

/// The canonical Prompt IR — a graph of segments that assembles into a request.
pub struct PromptIR {
    /// Segments in canonical order.
    segments: Vec<PromptSegment>,
    /// Message content by segment ID.
    message_data: std::collections::HashMap<SegmentId, Vec<Message>>,
    /// Tool schemas (sorted by name for canonical ordering).
    tool_schemas: Vec<ToolDeclaration>,
    /// System prompt text.
    system_prompt: String,
    /// Plan context text.
    plan_context: Option<String>,
    /// Repo map text.
    repo_map: Option<String>,
    /// Previous build's segment hashes (for dirty detection).
    prev_hashes: std::collections::HashMap<SegmentId, u64>,
}

impl PromptIR {
    pub fn new(system_prompt: String) -> Self {
        Self {
            segments: Vec::new(),
            message_data: std::collections::HashMap::new(),
            tool_schemas: Vec::new(),
            system_prompt,
            plan_context: None,
            repo_map: None,
            prev_hashes: std::collections::HashMap::new(),
        }
    }

    /// Set the plan/claim context.
    pub fn set_plan_context(&mut self, context: String) {
        self.plan_context = Some(context);
    }

    /// Set the repo map.
    pub fn set_repo_map(&mut self, map: String) {
        self.repo_map = Some(map);
    }

    /// Set tool schemas (will be canonically sorted).
    pub fn set_tools(&mut self, mut tools: Vec<ToolDeclaration>) {
        // Canonical ordering: sort by name
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        self.tool_schemas = tools;
    }

    /// Set the message history. Messages are partitioned into segments.
    pub fn set_messages(&mut self, messages: &[Message], preserve_recent: usize) {
        self.segments.clear();
        self.message_data.clear();

        // Segment 1: System prompt
        let sys_hash = hash_text(&self.system_prompt);
        let sys_id = sys_hash;
        self.segments.push(PromptSegment {
            id: sys_id,
            kind: SegmentKind::SystemPrompt,
            content_hash: sys_hash,
            token_count: estimate_tokens(&self.system_prompt),
            dirty: self.is_dirty(sys_id, sys_hash),
        });

        // Segment 2: Plan context (if any)
        if let Some(ref ctx) = self.plan_context {
            let h = hash_text(ctx);
            let id = h ^ 0x1111;
            self.segments.push(PromptSegment {
                id,
                kind: SegmentKind::PlanContext,
                content_hash: h,
                token_count: estimate_tokens(ctx),
                dirty: self.is_dirty(id, h),
            });
        }

        // Segment 3: Tool schemas
        let tools_str = self
            .tool_schemas
            .iter()
            .map(|t| format!("{}:{}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("|");
        let tools_hash = hash_text(&tools_str);
        let tools_id = tools_hash ^ 0x2222;
        self.segments.push(PromptSegment {
            id: tools_id,
            kind: SegmentKind::ToolSchemas,
            content_hash: tools_hash,
            token_count: (self.tool_schemas.len() as u64) * 50,
            dirty: self.is_dirty(tools_id, tools_hash),
        });

        // Segment 4: Repo map (if any)
        if let Some(ref map) = self.repo_map {
            let h = hash_text(map);
            let id = h ^ 0x3333;
            self.segments.push(PromptSegment {
                id,
                kind: SegmentKind::RepoMap,
                content_hash: h,
                token_count: estimate_tokens(map),
                dirty: self.is_dirty(id, h),
            });
        }

        // Segment 5+: Messages, partitioned into summary, history, recent
        if !messages.is_empty() {
            let recent_boundary = messages.len().saturating_sub(preserve_recent);

            // Summary messages (is_summary flag)
            let summaries: Vec<&Message> = messages[..recent_boundary]
                .iter()
                .filter(|m| m.metadata.is_summary)
                .collect();
            if !summaries.is_empty() {
                let h = hash_messages(&summaries);
                let id = h ^ 0x4444;
                let tokens = summaries.iter().map(|m| m.estimated_tokens()).sum();
                self.segments.push(PromptSegment {
                    id,
                    kind: SegmentKind::Summary,
                    content_hash: h,
                    token_count: tokens,
                    dirty: self.is_dirty(id, h),
                });
                self.message_data.insert(
                    id,
                    summaries.into_iter().cloned().collect(),
                );
            }

            // Historical (non-summary, non-recent)
            let historical: Vec<&Message> = messages[..recent_boundary]
                .iter()
                .filter(|m| !m.metadata.is_summary)
                .collect();
            if !historical.is_empty() {
                let h = hash_messages(&historical);
                let id = h ^ 0x5555;
                let tokens = historical.iter().map(|m| m.estimated_tokens()).sum();
                self.segments.push(PromptSegment {
                    id,
                    kind: SegmentKind::History,
                    content_hash: h,
                    token_count: tokens,
                    dirty: self.is_dirty(id, h),
                });
                self.message_data.insert(
                    id,
                    historical.into_iter().cloned().collect(),
                );
            }

            // Recent
            let recent: Vec<&Message> = messages[recent_boundary..].iter().collect();
            if !recent.is_empty() {
                let h = hash_messages(&recent);
                let id = h ^ 0x6666;
                let tokens = recent.iter().map(|m| m.estimated_tokens()).sum();
                self.segments.push(PromptSegment {
                    id,
                    kind: SegmentKind::Recent,
                    content_hash: h,
                    token_count: tokens,
                    dirty: self.is_dirty(id, h),
                });
                self.message_data.insert(
                    id,
                    recent.into_iter().cloned().collect(),
                );
            }
        }

        // Sort segments by kind (canonical order)
        self.segments.sort_by_key(|s| s.kind);
    }

    /// Get segments that changed since last build.
    pub fn dirty_segments(&self) -> Vec<&PromptSegment> {
        self.segments.iter().filter(|s| s.dirty).collect()
    }

    /// Get the total estimated token count.
    pub fn total_tokens(&self) -> u64 {
        self.segments.iter().map(|s| s.token_count).sum()
    }

    /// Get segments for cache breakpoint planning.
    pub fn cache_segments(&self) -> Vec<(SegmentKind, u64, bool)> {
        self.segments
            .iter()
            .map(|s| (s.kind, s.token_count, s.dirty))
            .collect()
    }

    /// Finalize: save current hashes as previous for next build.
    pub fn finalize(&mut self) {
        self.prev_hashes.clear();
        for seg in &self.segments {
            self.prev_hashes.insert(seg.id, seg.content_hash);
        }
    }

    /// Get all segments.
    pub fn segments(&self) -> &[PromptSegment] {
        &self.segments
    }

    fn is_dirty(&self, id: SegmentId, hash: u64) -> bool {
        self.prev_hashes.get(&id).map_or(true, |&prev| prev != hash)
    }
}

// ─── Hashing Utilities ──────────────────────────────────────────────────

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn hash_messages(messages: &[&Message]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for msg in messages {
        msg.text_content().hash(&mut hasher);
        format!("{:?}", msg.role).hash(&mut hasher);
    }
    hasher.finish()
}

fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let len = text.len();
    let punct = text.bytes().filter(|b| b.is_ascii_punctuation()).count();
    let ratio = punct as f64 / len as f64;
    let divisor = if ratio > 0.15 { 3.0 } else { 4.0 };
    (len as f64 / divisor) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_tool_ordering() {
        let mut ir = PromptIR::new("system".to_string());

        let tools = vec![
            ToolDeclaration {
                name: "zebra".to_string(),
                description: "z tool".to_string(),
                input_schema: serde_json::json!({}),
            },
            ToolDeclaration {
                name: "alpha".to_string(),
                description: "a tool".to_string(),
                input_schema: serde_json::json!({}),
            },
        ];

        ir.set_tools(tools);
        assert_eq!(ir.tool_schemas[0].name, "alpha");
        assert_eq!(ir.tool_schemas[1].name, "zebra");
    }

    #[test]
    fn dirty_detection() {
        let mut ir = PromptIR::new("system prompt v1".to_string());
        ir.set_messages(&[], 4);
        assert!(ir.segments.iter().all(|s| s.dirty)); // All dirty on first build
        ir.finalize();

        // Rebuild with same content — nothing dirty
        ir.set_messages(&[], 4);
        let dirty_count = ir.segments.iter().filter(|s| s.dirty).count();
        assert_eq!(dirty_count, 0);

        // Change system prompt — system segment becomes dirty
        ir.system_prompt = "system prompt v2".to_string();
        ir.set_messages(&[], 4);
        let dirty = ir.dirty_segments();
        assert!(dirty.iter().any(|s| s.kind == SegmentKind::SystemPrompt));
    }
}
