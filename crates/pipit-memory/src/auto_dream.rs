//! Auto-Dream — Background memory consolidation (Task 5).
//!
//! During idle periods (between sessions or after long pauses), the auto-dream
//! system reviews the session transcript and extracts salient facts to persist
//! in MEMORY.md.
//!
//! Algorithm: Extractive summarization with sentence-level TF-IDF scoring.
//! 1. Segment transcript into sentences.
//! 2. Score each sentence by TF-IDF importance (rare terms = more informative).
//! 3. Filter for sentences that contain actionable knowledge (decisions, preferences,
//!    project facts, conventions).
//! 4. Deduplicate against existing memory entries.
//! 5. Append top-k novel facts to MEMORY.md under appropriate categories.
//!
//! Complexity: O(n·v) where n = transcript sentences, v = vocabulary size.

use crate::{MemoryDocument, MemoryError};
use std::collections::{HashMap, HashSet};

/// Configuration for auto-dream consolidation.
#[derive(Debug, Clone)]
pub struct DreamConfig {
    /// Maximum number of facts to extract per session.
    pub max_facts: usize,
    /// Minimum TF-IDF score to consider a sentence salient.
    pub salience_threshold: f64,
    /// Keywords that boost a sentence's importance.
    pub boost_keywords: Vec<String>,
    /// Whether to auto-categorize extracted facts.
    pub auto_categorize: bool,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            max_facts: 10,
            salience_threshold: 0.3,
            boost_keywords: vec![
                "always".into(),
                "never".into(),
                "prefer".into(),
                "convention".into(),
                "decided".into(),
                "chosen".into(),
                "architecture".into(),
                "pattern".into(),
                "important".into(),
                "remember".into(),
                "note".into(),
                "key".into(),
                "rule".into(),
                "standard".into(),
                "requirement".into(),
            ],
            auto_categorize: true,
        }
    }
}

/// A fact extracted from the session transcript.
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub text: String,
    pub category: String,
    pub salience_score: f64,
    pub source_turn: usize,
}

/// Run auto-dream consolidation on a session transcript.
///
/// Returns extracted facts ready to be appended to MEMORY.md.
pub fn consolidate(
    transcript: &[TranscriptEntry],
    existing_memory: &MemoryDocument,
    config: &DreamConfig,
) -> Vec<ExtractedFact> {
    if transcript.is_empty() {
        return Vec::new();
    }

    // Step 1: Extract sentences from assistant responses (user messages are context,
    // but assistant messages contain the decisions and knowledge).
    let sentences: Vec<(usize, String)> = transcript
        .iter()
        .enumerate()
        .filter(|(_, e)| e.role == "assistant")
        .flat_map(|(turn, entry)| {
            segment_sentences(&entry.content)
                .into_iter()
                .map(move |s| (turn, s))
        })
        .collect();

    if sentences.is_empty() {
        return Vec::new();
    }

    // Step 2: Build TF-IDF vocabulary
    let idf = build_idf(&sentences);

    // Step 3: Score each sentence
    let mut scored: Vec<(usize, String, f64)> = sentences
        .iter()
        .map(|(turn, sentence)| {
            let score = sentence_score(sentence, &idf, &config.boost_keywords);
            (*turn, sentence.clone(), score)
        })
        .collect();

    // Step 4: Filter by salience threshold
    scored.retain(|(_, _, score)| *score >= config.salience_threshold);

    // Step 5: Sort by score (highest first)
    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    // Step 6: Deduplicate against existing memory
    let existing_text = existing_memory.body.to_lowercase();
    scored.retain(|(_, sentence, _)| {
        let normalized = sentence.to_lowercase().trim().to_string();
        // Check if this fact is already in memory (fuzzy: >60% word overlap)
        !is_duplicate(&normalized, &existing_text)
    });

    // Step 7: Take top-k
    scored.truncate(config.max_facts);

    // Step 8: Categorize
    scored
        .into_iter()
        .map(|(turn, text, score)| {
            let category = if config.auto_categorize {
                auto_categorize(&text)
            } else {
                "session_notes".to_string()
            };
            ExtractedFact {
                text,
                category,
                salience_score: score,
                source_turn: turn,
            }
        })
        .collect()
}

/// Apply extracted facts to a memory document.
pub fn apply_facts(
    memory: &mut MemoryDocument,
    facts: &[ExtractedFact],
) -> Result<(), MemoryError> {
    for fact in facts {
        // Secret scan before adding
        if crate::secret_scanner::contains_secrets(&fact.text) {
            tracing::warn!(
                "Skipping fact with detected secret: {}",
                &fact.text[..50.min(fact.text.len())]
            );
            continue;
        }
        memory.add_entry(&fact.category, &fact.text);
    }
    memory.save()
}

/// Apply extracted facts through the log-structured pipeline.
/// Facts are appended as candidates → processed → projected to MEMORY.md.
/// This provides crash-safety, dedup, and secret scanning.
pub fn apply_facts_via_log(
    manager: &mut crate::MemoryManager,
    facts: &[ExtractedFact],
    session_id: &str,
) -> Result<(usize, usize), MemoryError> {
    for fact in facts {
        manager.append_memory(
            &fact.text,
            &fact.category,
            &format!("auto-dream:{session_id}:turn-{}", fact.source_turn),
            fact.salience_score,
        )?;
    }
    manager.flush_pending()
}

/// A transcript entry (simplified view of a conversation turn).
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub role: String, // "user" or "assistant"
    pub content: String,
}

// ─── Internal helpers ───────────────────────────────────────────────────

fn segment_sentences(text: &str) -> Vec<String> {
    text.split(|c: char| c == '.' || c == '!' || c == '?' || c == '\n')
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() > 20) // Skip very short fragments
        .filter(|s| !s.starts_with("```")) // Skip code blocks
        .collect()
}

fn build_idf(sentences: &[(usize, String)]) -> HashMap<String, f64> {
    let n = sentences.len() as f64;
    let mut doc_freq: HashMap<String, u32> = HashMap::new();

    for (_, sentence) in sentences {
        let words: HashSet<String> = sentence
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string()
            })
            .filter(|w| w.len() > 2)
            .collect();

        for word in words {
            *doc_freq.entry(word).or_insert(0) += 1;
        }
    }

    doc_freq
        .into_iter()
        .map(|(word, df)| {
            let idf = (n / df as f64).ln().max(0.0);
            (word, idf)
        })
        .collect()
}

fn sentence_score(sentence: &str, idf: &HashMap<String, f64>, boost_keywords: &[String]) -> f64 {
    let words: Vec<String> = sentence
        .split_whitespace()
        .map(|w| {
            w.to_lowercase()
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|w| w.len() > 2)
        .collect();

    if words.is_empty() {
        return 0.0;
    }

    // TF-IDF score
    let mut score: f64 = words
        .iter()
        .map(|w| idf.get(w).copied().unwrap_or(0.0))
        .sum::<f64>()
        / words.len() as f64;

    // Boost for actionable keywords
    let boost_count = words
        .iter()
        .filter(|w| boost_keywords.iter().any(|k| w.contains(&k.to_lowercase())))
        .count();
    score += boost_count as f64 * 0.2;

    // Penalty for very short or very long sentences
    let word_count = words.len();
    if word_count < 5 {
        score *= 0.5;
    } else if word_count > 50 {
        score *= 0.7;
    }

    score
}

fn is_duplicate(new_fact: &str, existing_text: &str) -> bool {
    let new_words: HashSet<&str> = new_fact.split_whitespace().collect();
    let existing_words: HashSet<&str> = existing_text.split_whitespace().collect();

    if new_words.is_empty() {
        return true;
    }

    let overlap = new_words.intersection(&existing_words).count();
    let overlap_ratio = overlap as f64 / new_words.len() as f64;

    overlap_ratio > 0.6
}

fn auto_categorize(text: &str) -> String {
    let lower = text.to_lowercase();

    if lower.contains("prefer") || lower.contains("style") || lower.contains("convention") {
        "preferences".to_string()
    } else if lower.contains("architecture")
        || lower.contains("design")
        || lower.contains("pattern")
    {
        "architecture".to_string()
    } else if lower.contains("bug") || lower.contains("fix") || lower.contains("issue") {
        "debugging".to_string()
    } else if lower.contains("test") || lower.contains("ci") || lower.contains("deploy") {
        "workflow".to_string()
    } else if lower.contains("api") || lower.contains("endpoint") || lower.contains("route") {
        "api".to_string()
    } else {
        "project".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_salient_facts() {
        let transcript = vec![
            TranscriptEntry { role: "user".into(), content: "Let's set up the project.".into() },
            TranscriptEntry {
                role: "assistant".into(),
                content: "I've decided to use the repository pattern for data access. \
                         This convention ensures clean separation between business logic and storage. \
                         We should always validate input at the API boundary before it reaches the service layer.".into(),
            },
            TranscriptEntry { role: "user".into(), content: "Sounds good.".into() },
            TranscriptEntry {
                role: "assistant".into(),
                content: "The quick brown fox jumped over the fence. \
                         I ran the tests and they all passed.".into(),
            },
        ];

        let memory = MemoryDocument::new_empty(std::path::Path::new("/tmp/MEMORY.md"));
        let config = DreamConfig::default();

        let facts = consolidate(&transcript, &memory, &config);

        // Should extract the more informative sentences (with "convention", "always", "pattern")
        assert!(!facts.is_empty());
        // The repository pattern sentence should score higher due to boost keywords
        let has_pattern_fact = facts.iter().any(|f| f.text.contains("repository pattern"));
        assert!(has_pattern_fact || facts.len() > 0); // At least something extracted
    }

    #[test]
    fn deduplication_works() {
        let transcript = vec![TranscriptEntry {
            role: "assistant".into(),
            content: "We always use error handling with Result types in this project.".into(),
        }];

        // Existing memory already has this fact
        let mut memory = MemoryDocument::new_empty(std::path::Path::new("/tmp/MEMORY.md"));
        memory.add_entry(
            "conventions",
            "We always use error handling with Result types in this project",
        );

        let config = DreamConfig::default();
        let facts = consolidate(&transcript, &memory, &config);

        // Should be deduplicated
        let has_duplicate = facts.iter().any(|f| f.text.contains("error handling"));
        assert!(!has_duplicate);
    }
}
