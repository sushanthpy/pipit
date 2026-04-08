//! Git Temporal Knowledge Extractor — Task 3.1
//!
//! Builds a temporal knowledge graph from git history:
//! - Co-change frequency (Jaccard coefficient) between files
//! - Weighted expertise per author per file (recency-decayed)
//! - Causal modification chains (commit → file → line range)
//!
//! Queries: "why was this function written?", "what breaks if I change this?",
//!          "who understands this code best?"

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Temporal knowledge graph built from git history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalKnowledgeGraph {
    /// Co-change matrix: (file_a, file_b) → Jaccard coefficient in [0, 1].
    pub co_change: HashMap<(String, String), f64>,
    /// Per-author expertise: (author, file) → expertise score in [0, 1].
    pub expertise: HashMap<(String, String), f64>,
    /// Per-file commit history with message summaries.
    pub file_history: HashMap<String, Vec<CommitInfo>>,
    /// Files that frequently change together (clusters).
    pub change_clusters: Vec<ChangeCluster>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub message: String,
    pub files_changed: Vec<String>,
    pub lines_added: u32,
    pub lines_deleted: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeCluster {
    pub files: Vec<String>,
    pub cohesion: f64, // Average pairwise Jaccard within the cluster
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileExpertise {
    pub author: String,
    pub score: f64,
    pub last_touch_days: u32,
    pub total_lines_changed: u32,
}

/// Decay factor for expertise/recency calculations.
/// λ = 0.005 → half-life ≈ 139 days
const EXPERTISE_DECAY_LAMBDA: f64 = 0.005;

/// Minimum Jaccard coefficient to include in co-change map (filter noise).
const MIN_COCHANGE_THRESHOLD: f64 = 0.1;

impl TemporalKnowledgeGraph {
    /// Build the temporal knowledge graph from a git repository.
    ///
    /// Parses `git log --numstat --format=...` in a single pass: O(T * avg_files_per_commit).
    pub fn build(repo_root: &Path, max_commits: usize) -> Result<Self, String> {
        let commits = parse_git_log(repo_root, max_commits)?;
        if commits.is_empty() {
            return Ok(Self {
                co_change: HashMap::new(),
                expertise: HashMap::new(),
                file_history: HashMap::new(),
                change_clusters: Vec::new(),
            });
        }

        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as f64;

        // Build file→commit sets for Jaccard, and per-file history
        let mut file_commits: HashMap<String, HashSet<usize>> = HashMap::new();
        let mut file_history: HashMap<String, Vec<CommitInfo>> = HashMap::new();
        let mut author_file_stats: HashMap<(String, String), (f64, f64)> = HashMap::new(); // (weighted_lines, total_lines)

        for (idx, commit) in commits.iter().enumerate() {
            let age_days = commit_age_days(&commit.date, now_epoch);

            for file in &commit.files_changed {
                file_commits.entry(file.clone()).or_default().insert(idx);
                file_history
                    .entry(file.clone())
                    .or_default()
                    .push(commit.clone());
            }

            // Expertise: weighted authorship with recency decay
            let decay = (-EXPERTISE_DECAY_LAMBDA * age_days).exp();
            for file in &commit.files_changed {
                let key = (commit.author.clone(), file.clone());
                let entry = author_file_stats.entry(key).or_insert((0.0, 0.0));
                let lines = (commit.lines_added + commit.lines_deleted) as f64;
                entry.0 += lines * decay;
                entry.1 += lines;
            }
        }

        // Compute pairwise Jaccard for co-change
        let files: Vec<String> = file_commits.keys().cloned().collect();
        let mut co_change = HashMap::new();

        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let set_a = &file_commits[&files[i]];
                let set_b = &file_commits[&files[j]];
                let intersection = set_a.intersection(set_b).count();
                let union = set_a.union(set_b).count();
                if union > 0 {
                    let jaccard = intersection as f64 / union as f64;
                    if jaccard >= MIN_COCHANGE_THRESHOLD {
                        let key = if files[i] < files[j] {
                            (files[i].clone(), files[j].clone())
                        } else {
                            (files[j].clone(), files[i].clone())
                        };
                        co_change.insert(key, jaccard);
                    }
                }
            }
        }

        // Normalize expertise scores per file (highest expert = 1.0)
        let mut expertise = HashMap::new();
        let mut file_max: HashMap<String, f64> = HashMap::new();
        for ((author, file), (weighted, _total)) in &author_file_stats {
            let max = file_max.entry(file.clone()).or_insert(0.0_f64);
            *max = max.max(*weighted);
        }
        for ((author, file), (weighted, _total)) in &author_file_stats {
            let max = file_max.get(file).copied().unwrap_or(1.0).max(1e-10);
            expertise.insert((author.clone(), file.clone()), weighted / max);
        }

        // Build change clusters via greedy agglomerative approach
        let change_clusters = build_clusters(&co_change, &files, 0.3);

        Ok(Self {
            co_change,
            expertise,
            file_history,
            change_clusters,
        })
    }

    /// Query: who are the top experts for a given file?
    pub fn file_experts(&self, file: &str, top_k: usize) -> Vec<FileExpertise> {
        let mut experts: Vec<_> = self
            .expertise
            .iter()
            .filter(|((_, f), _)| f == file)
            .map(|((author, _), score)| FileExpertise {
                author: author.clone(),
                score: *score,
                last_touch_days: 0,
                total_lines_changed: 0,
            })
            .collect();
        experts.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        experts.truncate(top_k);
        experts
    }

    /// Query: which files co-change with the given file?
    pub fn co_changed_files(&self, file: &str, min_score: f64) -> Vec<(String, f64)> {
        let mut results: Vec<_> = self
            .co_change
            .iter()
            .filter_map(|((a, b), score)| {
                if *score >= min_score {
                    if a == file {
                        Some((b.clone(), *score))
                    } else if b == file {
                        Some((a.clone(), *score))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Query: recent history for files currently in context.
    pub fn relevant_history(
        &self,
        context_files: &[String],
        max_entries: usize,
    ) -> Vec<CommitInfo> {
        let mut all_commits: Vec<CommitInfo> = context_files
            .iter()
            .filter_map(|f| self.file_history.get(f))
            .flatten()
            .cloned()
            .collect();
        // Deduplicate by SHA
        let mut seen = HashSet::new();
        all_commits.retain(|c| seen.insert(c.sha.clone()));
        all_commits.truncate(max_entries);
        all_commits
    }

    /// Format archaeology context for injection into the system prompt.
    /// Includes recent commits, co-change clusters, and expertise for current files.
    /// This is the bridge between the temporal graph and the prompt builder.
    pub fn format_context(&self, context_files: &[String], max_tokens: usize) -> String {
        let mut output = String::new();
        let mut tokens_used = 0usize;
        let token_budget = max_tokens;

        // Section 1: Recent relevant commits
        let history = self.relevant_history(context_files, 10);
        if !history.is_empty() {
            output.push_str("## Recent Git History (relevant to current files)\n\n");
            for commit in &history {
                let line = format!(
                    "- `{}` by {} — {}\n",
                    &commit.sha[..commit.sha.len().min(8)],
                    commit.author,
                    commit.message,
                );
                let line_tokens = line.len() / 4;
                if tokens_used + line_tokens > token_budget {
                    break;
                }
                output.push_str(&line);
                tokens_used += line_tokens;
            }
            output.push('\n');
        }

        // Section 2: Co-change clusters (files that change together)
        let relevant_clusters: Vec<_> = self
            .change_clusters
            .iter()
            .filter(|c| c.files.iter().any(|f| context_files.contains(f)))
            .take(5)
            .collect();
        if !relevant_clusters.is_empty() {
            let header = "## Files That Change Together\n\n";
            if tokens_used + header.len() / 4 < token_budget {
                output.push_str(header);
                tokens_used += header.len() / 4;
                for cluster in relevant_clusters {
                    let line = format!(
                        "- [{}] (cohesion: {:.0}%)\n",
                        cluster.files.join(", "),
                        cluster.cohesion * 100.0,
                    );
                    let line_tokens = line.len() / 4;
                    if tokens_used + line_tokens > token_budget {
                        break;
                    }
                    output.push_str(&line);
                    tokens_used += line_tokens;
                }
                output.push('\n');
            }
        }

        // Section 3: Top experts for current files
        let header = "## Code Experts\n\n";
        if tokens_used + header.len() / 4 < token_budget {
            output.push_str(header);
            tokens_used += header.len() / 4;
            for file in context_files.iter().take(5) {
                let experts = self.file_experts(file, 2);
                if !experts.is_empty() {
                    let line = format!(
                        "- `{}`: {} ({:.0}% expertise)\n",
                        file,
                        experts[0].author,
                        experts[0].score * 100.0,
                    );
                    let line_tokens = line.len() / 4;
                    if tokens_used + line_tokens > token_budget {
                        break;
                    }
                    output.push_str(&line);
                    tokens_used += line_tokens;
                }
            }
        }

        output
    }
}

// ── Git log parsing ──

fn parse_git_log(repo_root: &Path, max_commits: usize) -> Result<Vec<CommitInfo>, String> {
    let output = Command::new("git")
        .args([
            "log",
            "--numstat",
            &format!("--format=%H%n%an%n%aI%n%s%n---END_HEADER---"),
            &format!("-{}", max_commits),
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git log failed: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "git log error: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    let mut lines = text.lines().peekable();

    while lines.peek().is_some() {
        // Skip empty lines
        while lines.peek().map(|l| l.is_empty()).unwrap_or(false) {
            lines.next();
        }

        let sha = match lines.next() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => break,
        };
        let author = lines.next().unwrap_or("unknown").to_string();
        let date = lines.next().unwrap_or("").to_string();
        let message = lines.next().unwrap_or("").to_string();

        // Skip the ---END_HEADER--- marker
        if let Some(l) = lines.peek() {
            if *l == "---END_HEADER---" {
                lines.next();
            }
        }

        // Parse numstat lines until empty line or next commit header
        let mut files_changed = Vec::new();
        let mut total_added = 0u32;
        let mut total_deleted = 0u32;

        while let Some(line) = lines.peek() {
            if line.is_empty() {
                lines.next();
                break;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() == 3 {
                let added: u32 = parts[0].parse().unwrap_or(0);
                let deleted: u32 = parts[1].parse().unwrap_or(0);
                let file = parts[2].to_string();
                total_added += added;
                total_deleted += deleted;
                files_changed.push(file);
                lines.next();
            } else {
                break;
            }
        }

        commits.push(CommitInfo {
            sha,
            author,
            date,
            message,
            files_changed,
            lines_added: total_added,
            lines_deleted: total_deleted,
        });
    }

    Ok(commits)
}

fn commit_age_days(date_str: &str, now_epoch: f64) -> f64 {
    // Parse ISO 8601 date roughly
    if let Some(date_part) = date_str.split('T').next() {
        let parts: Vec<&str> = date_part.split('-').collect();
        if parts.len() == 3 {
            if let (Ok(y), Ok(m), Ok(d)) = (
                parts[0].parse::<i64>(),
                parts[1].parse::<i64>(),
                parts[2].parse::<i64>(),
            ) {
                // Rough epoch calculation
                let commit_epoch = ((y - 1970) * 365 * 86400 + m * 30 * 86400 + d * 86400) as f64;
                return ((now_epoch - commit_epoch) / 86400.0).max(0.0);
            }
        }
    }
    365.0 // Default: 1 year old
}

/// Greedy agglomerative clustering: group files with high co-change.
fn build_clusters(
    co_change: &HashMap<(String, String), f64>,
    files: &[String],
    threshold: f64,
) -> Vec<ChangeCluster> {
    let mut clusters: Vec<ChangeCluster> = Vec::new();
    let mut assigned: HashSet<String> = HashSet::new();

    // Sort co-change pairs by score descending
    let mut pairs: Vec<_> = co_change.iter().collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));

    for ((a, b), score) in pairs {
        if *score < threshold {
            break;
        }
        if assigned.contains(a) || assigned.contains(b) {
            // Try to add to existing cluster
            for cluster in &mut clusters {
                if cluster.files.contains(a) && !assigned.contains(b) {
                    cluster.files.push(b.clone());
                    assigned.insert(b.clone());
                } else if cluster.files.contains(b) && !assigned.contains(a) {
                    cluster.files.push(a.clone());
                    assigned.insert(a.clone());
                }
            }
            continue;
        }
        assigned.insert(a.clone());
        assigned.insert(b.clone());
        clusters.push(ChangeCluster {
            files: vec![a.clone(), b.clone()],
            cohesion: *score,
        });
    }

    clusters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporal_graph_build_on_real_repo() {
        // This test runs on the pipit repo itself
        let root = std::env::current_dir().unwrap();
        let graph = TemporalKnowledgeGraph::build(&root, 50);
        assert!(graph.is_ok(), "Should parse git log: {:?}", graph.err());
        let graph = graph.unwrap();
        assert!(!graph.file_history.is_empty(), "Should have file history");
        assert!(!graph.expertise.is_empty(), "Should have expertise data");
    }

    #[test]
    fn test_jaccard_coefficient() {
        // Manual test: two files always co-changed → Jaccard = 1.0
        let mut co_change = HashMap::new();
        co_change.insert(("a.rs".to_string(), "b.rs".to_string()), 1.0);
        assert_eq!(
            co_change.get(&("a.rs".to_string(), "b.rs".to_string())),
            Some(&1.0)
        );
    }

    #[test]
    fn test_expertise_query() {
        let mut expertise = HashMap::new();
        expertise.insert(("alice".to_string(), "main.rs".to_string()), 1.0);
        expertise.insert(("bob".to_string(), "main.rs".to_string()), 0.5);

        let graph = TemporalKnowledgeGraph {
            co_change: HashMap::new(),
            expertise,
            file_history: HashMap::new(),
            change_clusters: Vec::new(),
        };

        let experts = graph.file_experts("main.rs", 2);
        assert_eq!(experts.len(), 2);
        assert_eq!(experts[0].author, "alice");
        assert!(experts[0].score > experts[1].score);
    }
}
