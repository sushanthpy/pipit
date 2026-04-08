//! Profile Ingestion — Task PGO-1
//!
//! Parses flamegraphs (folded stack format), perf output, memory profiles.
//! Hot path identification: top-k by sample count (Pareto ~20% functions = ~80% time).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A parsed profile report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileReport {
    pub source: String,
    pub total_samples: u64,
    pub hot_functions: Vec<HotFunction>,
    pub memory_allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotFunction {
    pub name: String,
    pub module: Option<String>,
    pub samples: u64,
    pub percentage: f64,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationSite {
    pub function: String,
    pub allocation_count: u64,
    pub peak_bytes: u64,
    /// Optimization opportunity: peak × frequency (allocation-in-loop signal).
    pub opportunity_score: f64,
}

/// Parse folded stack format (one line per sample: func_a;func_b;func_c 42).
pub fn parse_folded_stacks(input: &str) -> ProfileReport {
    let mut function_samples: HashMap<String, u64> = HashMap::new();
    let mut total_samples: u64 = 0;
    let mut caller_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut callee_map: HashMap<String, Vec<String>> = HashMap::new();

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Format: func_a;func_b;func_c 42
        let parts: Vec<&str> = line.rsplitn(2, ' ').collect();
        if parts.len() != 2 {
            continue;
        }

        let count: u64 = match parts[0].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        total_samples += count;

        let stack: Vec<&str> = parts[1].split(';').collect();
        if let Some(leaf) = stack.last() {
            *function_samples.entry(leaf.to_string()).or_default() += count;

            // Track caller/callee relationships
            for i in 0..stack.len() - 1 {
                callee_map
                    .entry(stack[i].to_string())
                    .or_default()
                    .push(stack[i + 1].to_string());
                caller_map
                    .entry(stack[i + 1].to_string())
                    .or_default()
                    .push(stack[i].to_string());
            }
        }
    }

    // Build sorted hot function list
    let mut hot_functions: Vec<HotFunction> = function_samples
        .iter()
        .map(|(name, &samples)| {
            let percentage = if total_samples > 0 {
                (samples as f64 / total_samples as f64) * 100.0
            } else {
                0.0
            };
            HotFunction {
                name: name.clone(),
                module: name.split("::").next().map(String::from),
                samples,
                percentage,
                callers: caller_map.get(name).cloned().unwrap_or_default(),
                callees: callee_map.get(name).cloned().unwrap_or_default(),
            }
        })
        .collect();
    hot_functions.sort_by(|a, b| b.samples.cmp(&a.samples));

    ProfileReport {
        source: "folded_stacks".into(),
        total_samples,
        hot_functions,
        memory_allocations: Vec::new(),
    }
}

/// Parse perf stat output for high-level metrics.
pub fn parse_perf_stat(input: &str) -> HashMap<String, f64> {
    let mut metrics = HashMap::new();
    for line in input.lines() {
        let parts: Vec<&str> = line.trim().splitn(2, ' ').collect();
        if parts.len() == 2 {
            if let Ok(val) = parts[0].replace(',', "").parse::<f64>() {
                metrics.insert(parts[1].trim().to_string(), val);
            }
        }
    }
    metrics
}

impl ProfileReport {
    /// Get methods consuming the top P% of samples.
    pub fn top_by_percentage(&self, threshold_pct: f64) -> Vec<&HotFunction> {
        let mut cumulative = 0.0;
        self.hot_functions
            .iter()
            .take_while(|f| {
                if cumulative >= threshold_pct {
                    return false;
                }
                cumulative += f.percentage;
                true
            })
            .collect()
    }

    /// Format as LLM-consumable summary.
    pub fn to_summary(&self, top_k: usize) -> String {
        let mut summary = format!("Profile Summary ({} total samples)\n\n", self.total_samples);
        summary.push_str("Top functions by CPU time:\n");
        for (i, func) in self.hot_functions.iter().take(top_k).enumerate() {
            summary.push_str(&format!(
                "  {}. {} — {:.1}% ({} samples)\n",
                i + 1,
                func.name,
                func.percentage,
                func.samples
            ));
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_folded_stacks() {
        let input = "main;process_request;parse_json 100\nmain;process_request;db_query 350\nmain;handle_auth 50\n";
        let report = parse_folded_stacks(input);
        assert_eq!(report.total_samples, 500);
        assert_eq!(report.hot_functions.len(), 3);
        assert_eq!(report.hot_functions[0].name, "db_query");
        assert_eq!(report.hot_functions[0].samples, 350);
        assert!((report.hot_functions[0].percentage - 70.0).abs() < 0.1);
    }

    #[test]
    fn test_top_by_percentage() {
        let input = "a 80\nb 10\nc 5\nd 3\ne 2\n";
        let report = parse_folded_stacks(input);
        let top80 = report.top_by_percentage(80.0);
        assert_eq!(top80.len(), 1, "Top 80% should be just 'a'");
        assert_eq!(top80[0].name, "a");
    }

    #[test]
    fn test_summary_format() {
        let input = "slow_function 90\nfast_function 10\n";
        let report = parse_folded_stacks(input);
        let summary = report.to_summary(5);
        assert!(summary.contains("slow_function"));
        assert!(summary.contains("90.0%"));
    }
}
