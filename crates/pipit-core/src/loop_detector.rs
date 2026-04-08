use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

/// Detect when the LLM gets stuck in a loop calling the same tools.
pub struct LoopDetector {
    history: VecDeque<ToolCallFingerprint>,
    window_size: usize,
    threshold: usize,
    /// Thinking text history for semantic loop detection.
    /// Tracks the model's reasoning across turns — if the text is >70% similar
    /// across 3 turns, the model is semantically stuck even if tool args differ.
    thinking_history: VecDeque<String>,
}

#[derive(Clone)]
struct ToolCallFingerprint {
    tool_name: String,
    args_hash: u64,
    token_set: Vec<String>,
    failed: bool,
}

impl Hash for ToolCallFingerprint {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.tool_name.hash(state);
        self.args_hash.hash(state);
    }
}

impl PartialEq for ToolCallFingerprint {
    fn eq(&self, other: &Self) -> bool {
        self.tool_name == other.tool_name && self.args_hash == other.args_hash
    }
}

impl Eq for ToolCallFingerprint {}

impl LoopDetector {
    pub fn new(window_size: usize, threshold: usize) -> Self {
        Self {
            history: VecDeque::new(),
            window_size,
            threshold,
            thinking_history: VecDeque::new(),
        }
    }

    pub fn record(&mut self, name: &str, args: &serde_json::Value) {
        let normalized_args = normalize_json(args).to_string();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        normalized_args.hash(&mut hasher);

        let fingerprint = ToolCallFingerprint {
            tool_name: name.to_string(),
            args_hash: hasher.finish(),
            token_set: tokenize(&normalized_args),
            failed: false,
        };

        self.history.push_back(fingerprint);
        if self.history.len() > self.window_size {
            self.history.pop_front();
        }
    }

    /// Mark the most recent call for a given tool as failed.
    /// Only failed calls count toward loop detection.
    pub fn mark_last_failed(&mut self, name: &str) {
        if let Some(fp) = self
            .history
            .iter_mut()
            .rev()
            .find(|fp| fp.tool_name == name)
        {
            fp.failed = true;
        }
    }

    /// Clear the history — call this when the agent makes forward progress
    /// (e.g. a successful mutating tool call) to avoid stale entries
    /// from triggering false positives.
    pub fn reset(&mut self) {
        self.history.clear();
        self.thinking_history.clear();
    }

    /// Record the model's thinking/response text for this turn.
    /// Used by the semantic loop detector.
    pub fn record_thinking(&mut self, text: &str) {
        // Normalize: collapse whitespace, lowercase, trim to first 300 chars
        let normalized: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .chars()
            .take(300)
            .collect();
        self.thinking_history.push_back(normalized);
        if self.thinking_history.len() > 10 {
            self.thinking_history.pop_front();
        }
    }

    /// Check if the model is semantically stuck — same reasoning text
    /// repeated across recent turns even though tool args may differ.
    ///
    /// Returns Some(count) if ≥2 of the last 3 thinking blocks are >70% similar
    /// to the current one (via normalized Levenshtein distance).
    pub fn check_semantic_loop(&self) -> Option<u32> {
        if self.thinking_history.len() < 3 {
            return None;
        }
        let current = self.thinking_history.back()?;
        if current.is_empty() {
            return None;
        }
        let recent: Vec<&String> = self
            .thinking_history
            .iter()
            .rev()
            .skip(1) // skip current
            .take(3)
            .collect();
        let similar_count = recent
            .iter()
            .filter(|prev| normalized_levenshtein(prev, current) > 0.70)
            .count() as u32;
        if similar_count >= 2 {
            Some(similar_count)
        } else {
            None
        }
    }

    /// Check if any tool+args combo has been called >= threshold times with failures.
    /// Only failed calls count — successful calls are normal agent behavior.
    pub fn is_looping(&self) -> Option<(String, u32)> {
        // Only consider failed calls for loop detection
        let failed_calls: Vec<_> = self.history.iter().filter(|fp| fp.failed).collect();

        let mut counts: HashMap<(&str, u64), u32> = HashMap::new();
        for fp in &failed_calls {
            *counts.entry((&fp.tool_name, fp.args_hash)).or_default() += 1;
        }

        if let Some(exact) = counts
            .iter()
            .find(|(_, count)| **count >= self.threshold as u32)
            .map(|((name, _), count)| (name.to_string(), *count))
        {
            return Some(exact);
        }

        // Fuzzy matching on failed calls only
        let mut best_match: Option<(String, u32)> = None;
        for current in &failed_calls {
            let similar = failed_calls
                .iter()
                .filter(|candidate| {
                    candidate.tool_name == current.tool_name
                        && jaccard_similarity(&candidate.token_set, &current.token_set) >= 0.82
                })
                .count() as u32;

            if similar >= self.threshold as u32 {
                match &best_match {
                    Some((_, best_count)) if *best_count >= similar => {}
                    _ => best_match = Some((current.tool_name.clone(), similar)),
                }
            }
        }

        best_match
    }
}

fn normalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));

            let normalized = entries
                .into_iter()
                .map(|(key, value)| (key.clone(), normalize_json(value)))
                .collect();

            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(normalize_json).collect())
        }
        serde_json::Value::String(text) => serde_json::Value::String(normalize_string(text)),
        _ => value.clone(),
    }
}

fn normalize_string(input: &str) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let path_normalized = collapsed
        .replace("\\", "/")
        .replace("/./", "/")
        .trim_start_matches("./")
        .to_string();

    path_normalized
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '/' && c != '.')
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn jaccard_similarity(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }

    let intersection = left.iter().filter(|token| right.contains(token)).count() as f64;
    let union = (left.len() + right.len()) as f64 - intersection;

    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// Normalized Levenshtein similarity: 1.0 = identical, 0.0 = completely different.
/// Computes edit distance / max(len_a, len_b) and returns 1.0 - that ratio.
/// O(m·n) but inputs are bounded to 300 chars (~90k operations max).
fn normalized_levenshtein(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 && n == 0 {
        return 1.0;
    }
    if m == 0 || n == 0 {
        return 0.0;
    }

    // Single-row DP for space efficiency
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    let dist = prev[n] as f64;
    let max_len = m.max(n) as f64;
    1.0 - (dist / max_len)
}
