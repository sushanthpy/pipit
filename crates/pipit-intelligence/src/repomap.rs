use crate::discovery;
use crate::graph::ReferenceGraph;
use crate::tags::{self, FileTag, TagKind};
use crate::IntelligenceConfig;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Cached tag data with per-file mtime tracking.
#[derive(Serialize, Deserialize)]
struct TagCache {
    /// Tags keyed by relative file path.
    file_tags: HashMap<PathBuf, Vec<FileTag>>,
    /// Mtime (seconds since epoch) per file.
    file_mtimes: HashMap<PathBuf, u64>,
}

/// The complete RepoMap: structural understanding of the codebase.
pub struct RepoMap {
    root: PathBuf,
    config: IntelligenceConfig,
    graph: ReferenceGraph,
    files: Vec<PathBuf>,
    dirty_files: HashSet<PathBuf>,
}

pub struct RankedFile {
    pub path: PathBuf,
    pub rank: f64,
    pub definitions: Vec<FileTag>,
}

impl RepoMap {
    /// Build a RepoMap, using cached tags when available.
    /// The cache is stored at `.pipit/repomap.cache` and
    /// invalidated per-file based on mtime.
    pub fn build(root: &Path, config: IntelligenceConfig) -> Self {
        let files = discovery::discover_files(root, config.max_file_size);
        let cache_path = root.join(".pipit").join("repomap.cache");

        // Try loading the cache
        let cached = Self::load_cache(&cache_path);

        // Determine which files need re-parsing
        let mut cached_tags: HashMap<PathBuf, Vec<FileTag>> = cached
            .as_ref()
            .map(|c| c.file_tags.clone())
            .unwrap_or_default();
        let cached_mtimes: HashMap<PathBuf, u64> = cached
            .map(|c| c.file_mtimes)
            .unwrap_or_default();

        let mut dirty: Vec<PathBuf> = Vec::new();
        let mut current_mtimes: HashMap<PathBuf, u64> = HashMap::new();

        for file in &files {
            let abs = root.join(file);
            let mtime = abs.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            current_mtimes.insert(file.clone(), mtime);

            let needs_reparse = cached_mtimes.get(file)
                .map(|&cached_mtime| cached_mtime != mtime)
                .unwrap_or(true);

            if needs_reparse {
                dirty.push(file.clone());
            }
        }

        // Remove tags for files that no longer exist
        let file_set: HashSet<&PathBuf> = files.iter().collect();
        cached_tags.retain(|k, _| file_set.contains(k));

        // Only re-parse dirty files
        if !dirty.is_empty() {
            let new_tags = tags::extract_all_tags(root, &dirty);
            // Group new tags by file
            let mut by_file: HashMap<PathBuf, Vec<FileTag>> = HashMap::new();
            for tag in new_tags {
                by_file.entry(tag.rel_path.clone()).or_default().push(tag);
            }
            // Merge into cached tags
            for (file, file_tags) in by_file {
                cached_tags.insert(file, file_tags);
            }
        }

        // Flatten all tags and build graph
        let all_tags: Vec<FileTag> = cached_tags.values().flatten().cloned().collect();
        let graph = ReferenceGraph::build(&all_tags);

        // Persist updated cache
        let new_cache = TagCache {
            file_tags: cached_tags,
            file_mtimes: current_mtimes,
        };
        Self::save_cache(&cache_path, &new_cache);

        Self {
            root: root.to_path_buf(),
            config,
            graph,
            files,
            dirty_files: HashSet::new(),
        }
    }

    fn load_cache(path: &Path) -> Option<TagCache> {
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_cache(path: &Path, cache: &TagCache) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string(cache) {
            let _ = std::fs::write(path, data);
        }
    }

    /// Refresh only dirty files (incremental update).
    pub fn refresh(&mut self) {
        if self.dirty_files.is_empty() {
            return;
        }

        let dirty: Vec<PathBuf> = self.dirty_files.drain().collect();
        let new_tags = tags::extract_all_tags(&self.root, &dirty);
        let dirty_set: HashSet<PathBuf> = dirty.into_iter().collect();
        self.graph.update_files(&dirty_set, &new_tags);
    }

    /// Mark a file as modified (needs re-parsing on next refresh).
    pub fn mark_dirty(&mut self, path: PathBuf) {
        self.dirty_files.insert(path);
    }

    /// Rank files by importance with optional boosting.
    pub fn rank_files(
        &self,
        context_files: &[PathBuf],
        chat_mentions: &[String],
    ) -> Vec<RankedFile> {
        let page_ranks = self.graph.page_rank();

        let mut ranked: Vec<RankedFile> = self
            .files
            .iter()
            .map(|path| {
                let base_rank = page_ranks.get(path).copied().unwrap_or(0.001);

                // Boost files already in context
                let context_boost = if context_files.contains(path) {
                    2.0
                } else {
                    1.0
                };

                // Boost files containing mentioned symbols
                let mention_boost = chat_mentions
                    .iter()
                    .filter(|sym| {
                        self.graph.tags.iter().any(|t| {
                            t.rel_path == *path
                                && t.name == **sym
                                && t.kind == TagKind::Definition
                        })
                    })
                    .count() as f64
                    * 1.5
                    + 1.0;

                let definitions: Vec<FileTag> = self
                    .graph
                    .definitions_in(path)
                    .into_iter()
                    .cloned()
                    .collect();

                RankedFile {
                    path: path.clone(),
                    rank: base_rank * context_boost * mention_boost,
                    definitions,
                }
            })
            .collect();

        ranked.sort_by(|a, b| b.rank.partial_cmp(&a.rank).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Render the repo map as a condensed string for the system prompt.
    pub fn render(&self, context_files: &[PathBuf], token_budget: usize) -> String {
        let ranked = self.rank_files(context_files, &[]);

        let mut output = String::from("# Repository Structure\n\n");
        /// Rough bytes-per-token ratio for English/code text (1 token ≈ 4 bytes on average).
        const BYTES_PER_TOKEN: usize = 4;

        let mut estimated_tokens = output.len() / BYTES_PER_TOKEN;

        for file in &ranked {
            let section = render_file_section(&self.root, file);
            let section_tokens = section.len() / BYTES_PER_TOKEN;

            if estimated_tokens + section_tokens > token_budget {
                break;
            }

            output.push_str(&section);
            estimated_tokens += section_tokens;
        }

        output
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

fn render_file_section(root: &Path, file: &RankedFile) -> String {
    if file.definitions.is_empty() {
        return format!("- {}\n", file.path.display());
    }

    let abs_path = root.join(&file.path);
    let source = match std::fs::read_to_string(&abs_path) {
        Ok(s) => s,
        Err(_) => return format!("- {}\n", file.path.display()),
    };

    let lines: Vec<&str> = source.lines().collect();
    let mut section = format!("## {}\n```\n", file.path.display());

    for def in &file.definitions {
        let line_idx = (def.line as usize).saturating_sub(1);
        if line_idx < lines.len() {
            let start = line_idx;
            let end = (line_idx + 2).min(lines.len());
            for i in start..end {
                section.push_str(lines[i]);
                section.push('\n');
            }
            section.push_str("    ...\n\n");
        }
    }

    section.push_str("```\n\n");
    section
}
