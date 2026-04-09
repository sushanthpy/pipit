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

    /// Check if any tool+args combo has been called >= threshold times.
    ///
    /// Detection runs in three phases, tightest first:
    ///
    /// 1. **Exact duplicates (all calls):** Same tool name + identical args hash.
    ///    History is `reset()` on every forward-progress mutation, so everything
    ///    remaining is from stagnant turns.  Repeating the same read-only command
    ///    (e.g. `grep` on the same file 5×) is a loop even when calls succeed.
    ///
    /// 2. **Fuzzy duplicates (failed calls):** Same tool name, ≥82% Jaccard
    ///    similarity on arg tokens, but only counting calls that returned an
    ///    error or were policy-blocked.
    ///
    /// 3. **Fuzzy duplicates (all calls, higher bar):** Same as phase 2 but
    ///    on all calls including successes, with threshold + 2 (min 5).
    pub fn is_looping(&self) -> Option<(String, u32)> {
        // ── Phase 1: exact duplicates on ALL calls ──
        let mut exact_counts: HashMap<(&str, u64), u32> = HashMap::new();
        for fp in &self.history {
            *exact_counts.entry((&fp.tool_name, fp.args_hash)).or_default() += 1;
        }

        if let Some(exact) = exact_counts
            .iter()
            .find(|(_, count)| **count >= self.threshold as u32)
            .map(|((name, _), count)| (name.to_string(), *count))
        {
            return Some(exact);
        }

        // ── Phase 2: fuzzy duplicates on FAILED calls ──
        let failed_calls: Vec<_> = self.history.iter().filter(|fp| fp.failed).collect();
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
        if best_match.is_some() {
            return best_match;
        }

        // ── Phase 3: fuzzy duplicates on ALL calls (higher bar) ──
        // Catches models that slightly vary args on read-only calls
        // (e.g. grep -n "export" vs grep -n 'export').
        let fuzzy_all_threshold = (self.threshold as u32 + 2).max(5);
        for current in self.history.iter() {
            let similar = self.history
                .iter()
                .filter(|candidate| {
                    candidate.tool_name == current.tool_name
                        && jaccard_similarity(&candidate.token_set, &current.token_set) >= 0.82
                })
                .count() as u32;

            if similar >= fuzzy_all_threshold {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Successful identical calls must be detected as a loop.
    /// This is the grep-on-same-file-100-times scenario.
    #[test]
    fn detects_successful_identical_calls() {
        let mut ld = LoopDetector::new(10, 3);
        let args = json!({"command": "grep -n \"export default\" /tmp/foo.txt"});

        // Call the same command 3 times successfully (never marked failed)
        ld.record("Bash", &args);
        ld.record("Bash", &args);
        assert!(ld.is_looping().is_none(), "2 calls should be below threshold");
        ld.record("Bash", &args);
        let result = ld.is_looping();
        assert!(result.is_some(), "3 identical successful calls should be detected");
        let (name, count) = result.unwrap();
        assert_eq!(name, "Bash");
        assert_eq!(count, 3);
    }

    /// Mutation resets history, so post-mutation reads should not count
    /// against pre-mutation reads.
    #[test]
    fn reset_clears_successful_call_history() {
        let mut ld = LoopDetector::new(10, 3);
        let args = json!({"command": "grep foo bar.txt"});

        ld.record("Bash", &args);
        ld.record("Bash", &args);
        ld.reset(); // mutation happened
        ld.record("Bash", &args);
        assert!(ld.is_looping().is_none(), "should not loop after reset");
    }

    /// Failed calls still detected at normal threshold (existing behavior).
    #[test]
    fn detects_failed_call_loop() {
        let mut ld = LoopDetector::new(10, 3);
        let args = json!({"path": "/tmp/nonexistent"});

        for _ in 0..3 {
            ld.record("ReadFile", &args);
            ld.mark_last_failed("ReadFile");
        }
        let result = ld.is_looping();
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "ReadFile");
    }

    /// Different args should not trigger exact-match detection.
    #[test]
    fn different_args_no_exact_loop() {
        let mut ld = LoopDetector::new(10, 3);

        ld.record("Bash", &json!({"command": "grep foo a.txt"}));
        ld.record("Bash", &json!({"command": "grep foo b.txt"}));
        ld.record("Bash", &json!({"command": "grep foo c.txt"}));
        assert!(ld.is_looping().is_none(), "different args should not exact-match");
    }

    /// Semantic loop detection on thinking text.
    #[test]
    fn detects_semantic_loop() {
        let mut ld = LoopDetector::new(10, 3);
        let text = "Now I have enough information to create a comprehensive markdown file";

        ld.record_thinking(text);
        ld.record_thinking(text);
        ld.record_thinking(text);
        ld.record_thinking(text);
        assert!(ld.check_semantic_loop().is_some());
    }
}
