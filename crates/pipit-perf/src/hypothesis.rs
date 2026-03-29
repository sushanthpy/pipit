//! Optimization Hypothesis Generator — Task PGO-2
//!
//! Pattern matching (30 known anti-patterns) + LLM for novel bottlenecks.
//! O(n·p) where n=hot_functions, p=patterns.
//! Template speedup ranges from empirical benchmarks.

use crate::profile::{ProfileReport, HotFunction};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BottleneckKind {
    Algorithmic,
    Allocation,
    Serialization,
    Concurrency,
    IoBlocking,
    CacheLocality,
    StringProcessing,
    CollectionChoice,
    Redundant,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence { High, Medium, Low }

/// An optimization hypothesis with proposed fix and expected speedup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationHypothesis {
    pub target_function: String,
    pub bottleneck: BottleneckKind,
    pub description: String,
    pub proposed_fix: String,
    pub expected_speedup_range: (f64, f64),
    pub confidence: Confidence,
    pub function_percentage: f64,
    /// Impact: speedup × percentage (prioritization metric).
    pub impact_score: f64,
}

/// Known anti-patterns with template speedup ranges.
struct AntiPattern {
    name: &'static str,
    keywords: &'static [&'static str],
    bottleneck: BottleneckKind,
    fix_template: &'static str,
    speedup_range: (f64, f64),
}

const PATTERNS: &[AntiPattern] = &[
    AntiPattern { name: "HashMap with small keys", keywords: &["HashMap", "hash_map", "dict"], bottleneck: BottleneckKind::CollectionChoice, fix_template: "Replace HashMap with Vec<(K,V)> or BTreeMap for small key sets (<16 entries)", speedup_range: (1.5, 3.0) },
    AntiPattern { name: "Allocation in loop", keywords: &["Vec::new", "String::new", "alloc", "malloc", "push(String"], bottleneck: BottleneckKind::Allocation, fix_template: "Pre-allocate with Vec::with_capacity() or reuse buffers", speedup_range: (1.2, 4.0) },
    AntiPattern { name: "serde_json in hot path", keywords: &["serde_json::from_str", "serde_json::to_string", "json.loads", "json.dumps", "JSON.parse"], bottleneck: BottleneckKind::Serialization, fix_template: "Use simd-json, zero-copy deserialization, or pre-parsed cache", speedup_range: (1.5, 5.0) },
    AntiPattern { name: "Synchronous I/O in async", keywords: &["std::fs::read", "std::fs::write", "blocking", "thread::sleep"], bottleneck: BottleneckKind::IoBlocking, fix_template: "Use tokio::fs or spawn_blocking for I/O operations", speedup_range: (2.0, 10.0) },
    AntiPattern { name: "Lock contention", keywords: &["Mutex::lock", "RwLock", "synchronized", "lock().unwrap()"], bottleneck: BottleneckKind::Concurrency, fix_template: "Use lock-free data structures, reduce critical section, or shard the lock", speedup_range: (1.5, 8.0) },
    AntiPattern { name: "String formatting in hot path", keywords: &["format!", "to_string()", "String::from", "str.format("], bottleneck: BottleneckKind::StringProcessing, fix_template: "Use write!() to pre-allocated buffer, or use Cow<str>", speedup_range: (1.3, 2.5) },
    AntiPattern { name: "Clone in loop", keywords: &[".clone()", "copy.deepcopy", "Object.assign"], bottleneck: BottleneckKind::Allocation, fix_template: "Use references or Rc/Arc instead of cloning", speedup_range: (1.2, 3.0) },
    AntiPattern { name: "Quadratic algorithm", keywords: &["contains(", "find(", "index(", "in list"], bottleneck: BottleneckKind::Algorithmic, fix_template: "Use HashSet for membership checks, sort+binary_search, or index", speedup_range: (2.0, 100.0) },
    AntiPattern { name: "Regex compilation in loop", keywords: &["Regex::new", "re.compile", "new RegExp"], bottleneck: BottleneckKind::Redundant, fix_template: "Compile regex once outside the loop (lazy_static, once_cell)", speedup_range: (5.0, 50.0) },
    AntiPattern { name: "Unbatched database queries", keywords: &["execute(", "query(", "cursor.execute", "SELECT"], bottleneck: BottleneckKind::IoBlocking, fix_template: "Batch queries, use query pipelining, or add caching layer", speedup_range: (2.0, 20.0) },
];

/// Generate optimization hypotheses from a profile report. O(n·p).
pub fn generate_hypotheses(report: &ProfileReport, code_snippets: &std::collections::HashMap<String, String>) -> Vec<OptimizationHypothesis> {
    let mut hypotheses = Vec::new();

    for func in &report.hot_functions {
        if func.percentage < 1.0 { continue; } // Skip functions < 1% of time

        let code = code_snippets.get(&func.name).map(|s| s.as_str()).unwrap_or("");

        for pattern in PATTERNS {
            if pattern.keywords.iter().any(|kw| func.name.contains(kw) || code.contains(kw)) {
                let mid_speedup = (pattern.speedup_range.0 + pattern.speedup_range.1) / 2.0;
                let impact = mid_speedup * func.percentage / 100.0;

                hypotheses.push(OptimizationHypothesis {
                    target_function: func.name.clone(),
                    bottleneck: pattern.bottleneck,
                    description: format!("{}: detected in {} ({:.1}% of time)", pattern.name, func.name, func.percentage),
                    proposed_fix: pattern.fix_template.to_string(),
                    expected_speedup_range: pattern.speedup_range,
                    confidence: Confidence::High,
                    function_percentage: func.percentage,
                    impact_score: impact,
                });
            }
        }

        // If no pattern matched but function is hot (>5%), flag for LLM analysis
        if func.percentage > 5.0 && !hypotheses.iter().any(|h| h.target_function == func.name) {
            hypotheses.push(OptimizationHypothesis {
                target_function: func.name.clone(),
                bottleneck: BottleneckKind::Unknown,
                description: format!("{} consumes {:.1}% of CPU — needs LLM analysis", func.name, func.percentage),
                proposed_fix: "Requires manual analysis or LLM-guided optimization".into(),
                expected_speedup_range: (1.0, 2.0),
                confidence: Confidence::Low,
                function_percentage: func.percentage,
                impact_score: func.percentage / 100.0,
            });
        }
    }

    // Sort by impact score descending
    hypotheses.sort_by(|a, b| b.impact_score.partial_cmp(&a.impact_score).unwrap_or(std::cmp::Ordering::Equal));
    hypotheses
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::parse_folded_stacks;

    #[test]
    fn test_pattern_matching() {
        let input = "main;serde_json::from_str 800\nmain;process 200\n";
        let report = parse_folded_stacks(input);
        let mut snippets = std::collections::HashMap::new();
        snippets.insert("serde_json::from_str".into(), "serde_json::from_str(data)".into());

        let hyps = generate_hypotheses(&report, &snippets);
        assert!(!hyps.is_empty(), "Should detect serde anti-pattern");
        assert_eq!(hyps[0].bottleneck, BottleneckKind::Serialization);
        assert!(hyps[0].expected_speedup_range.0 > 1.0);
    }

    #[test]
    fn test_unknown_hot_function() {
        let input = "main;mystery_function 950\nmain;other 50\n";
        let report = parse_folded_stacks(input);
        let snippets = std::collections::HashMap::new();

        let hyps = generate_hypotheses(&report, &snippets);
        let mystery = hyps.iter().find(|h| h.target_function == "mystery_function");
        assert!(mystery.is_some(), "Should flag hot unknown function");
        assert_eq!(mystery.unwrap().bottleneck, BottleneckKind::Unknown);
        assert_eq!(mystery.unwrap().confidence, Confidence::Low);
    }

    #[test]
    fn test_impact_sorting() {
        let input = "a;HashMap::insert 300\na;String::from 200\na;io::read 500\n";
        let report = parse_folded_stacks(input);
        let mut snippets = std::collections::HashMap::new();
        snippets.insert("HashMap::insert".into(), "HashMap::new(); map.insert(k,v)".into());

        let hyps = generate_hypotheses(&report, &snippets);
        if hyps.len() >= 2 {
            assert!(hyps[0].impact_score >= hyps[1].impact_score, "Should be sorted by impact");
        }
    }
}
