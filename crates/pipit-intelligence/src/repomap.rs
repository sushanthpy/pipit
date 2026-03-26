use crate::discovery;
use crate::graph::ReferenceGraph;
use crate::tags::{self, FileTag, TagKind};
use crate::IntelligenceConfig;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    /// Build a RepoMap from scratch.
    pub fn build(root: &Path, config: IntelligenceConfig) -> Self {
        let files = discovery::discover_files(root, config.max_file_size);
        let all_tags = tags::extract_all_tags(root, &files);
        let graph = ReferenceGraph::build(&all_tags);

        Self {
            root: root.to_path_buf(),
            config,
            graph,
            files,
            dirty_files: HashSet::new(),
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
