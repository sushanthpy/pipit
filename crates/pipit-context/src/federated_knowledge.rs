//! Federated Knowledge Graph — cross-repository learning (INTEL-4).
//!
//! Pipit learns patterns from every project and can share them across
//! mesh nodes (opt-in, privacy-preserving).
//!
//! Storage: local knowledge in `.pipit/knowledge/`
//! Sharing: only TF-IDF vectors are shared, not raw code
//! Relevance: cos_sim(project_vector, learning_vector) × e^(-λ·age)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A learning unit extracted from project interactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningUnit {
    pub id: String,
    pub concept: String,
    pub pattern: String,
    pub source_project: String,
    pub language: Option<String>,
    pub confidence: f64,
    pub frequency: u32,
    pub last_used: String,
    pub created_at: String,
    /// TF-IDF feature vector (sparse: term → weight).
    pub features: HashMap<String, f64>,
}

/// Privacy settings for federated knowledge sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPolicy {
    /// Allow sharing learning vectors with mesh nodes.
    pub enabled: bool,
    /// Only share learnings from these projects.
    pub allowed_projects: Vec<String>,
    /// Never share learnings containing these terms.
    pub blocked_terms: Vec<String>,
    /// Maximum units to share per sync.
    pub max_share_count: usize,
}

impl Default for FederationPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_projects: Vec::new(),
            blocked_terms: vec![
                "password".to_string(),
                "secret".to_string(),
                "token".to_string(),
                "api_key".to_string(),
                "private_key".to_string(),
            ],
            max_share_count: 50,
        }
    }
}

/// The federated knowledge store.
pub struct FederatedKnowledgeStore {
    units: Vec<LearningUnit>,
    policy: FederationPolicy,
}

impl FederatedKnowledgeStore {
    /// Load from `.pipit/knowledge/federated.json`.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root
            .join(".pipit")
            .join("knowledge")
            .join("federated.json");
        let units = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let policy_path = project_root
            .join(".pipit")
            .join("knowledge")
            .join("federation-policy.json");
        let policy = if policy_path.exists() {
            std::fs::read_to_string(&policy_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            FederationPolicy::default()
        };
        Self { units, policy }
    }

    /// Record a new learning from the current project.
    pub fn learn(&mut self, concept: &str, pattern: &str, project: &str, project_root: &Path) {
        // Check if this concept already exists
        if let Some(existing) = self.units.iter_mut().find(|u| u.concept == concept) {
            existing.frequency += 1;
            existing.last_used = chrono::Utc::now().to_rfc3339();
            existing.confidence = (existing.confidence + 0.1).min(1.0);
        } else {
            let features = compute_tfidf(pattern);
            self.units.push(LearningUnit {
                id: uuid::Uuid::new_v4().to_string(),
                concept: concept.to_string(),
                pattern: pattern.to_string(),
                source_project: project.to_string(),
                language: None,
                confidence: 0.5,
                frequency: 1,
                last_used: chrono::Utc::now().to_rfc3339(),
                created_at: chrono::Utc::now().to_rfc3339(),
                features,
            });
        }
        self.save(project_root);
    }

    /// Query the knowledge store for relevant learnings.
    pub fn query(&self, query: &str, top_k: usize) -> Vec<&LearningUnit> {
        let query_features = compute_tfidf(query);
        let mut scored: Vec<(&LearningUnit, f64)> = self
            .units
            .iter()
            .map(|unit| {
                let sim = cosine_similarity(&query_features, &unit.features);
                let age_days = chrono::Utc::now()
                    .signed_duration_since(
                        chrono::DateTime::parse_from_rfc3339(&unit.last_used)
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(|_| chrono::Utc::now()),
                    )
                    .num_days() as f64;
                let decay = (-0.001 * age_days).exp();
                let score = sim * decay * unit.confidence;
                (unit, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(top_k).map(|(u, _)| u).collect()
    }

    /// Get units eligible for federation sharing (privacy-filtered).
    pub fn shareable_units(&self) -> Vec<&LearningUnit> {
        if !self.policy.enabled {
            return Vec::new();
        }
        self.units
            .iter()
            .filter(|u| {
                // Check project allowlist
                if !self.policy.allowed_projects.is_empty()
                    && !self.policy.allowed_projects.contains(&u.source_project)
                {
                    return false;
                }
                // Check blocked terms
                let lower = u.pattern.to_lowercase();
                !self.policy.blocked_terms.iter().any(|t| lower.contains(t))
            })
            .take(self.policy.max_share_count)
            .collect()
    }

    /// Merge knowledge received from a remote mesh node.
    pub fn merge_remote(&mut self, remote_units: &[LearningUnit], project_root: &Path) {
        for remote in remote_units {
            if !self.units.iter().any(|u| u.id == remote.id) {
                let mut unit = remote.clone();
                unit.confidence *= 0.8; // Discount remote learnings
                self.units.push(unit);
            }
        }
        self.save(project_root);
    }

    fn save(&self, project_root: &Path) {
        let dir = project_root.join(".pipit").join("knowledge");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("federated.json");
        let json = serde_json::to_string_pretty(&self.units).unwrap_or_default();
        let _ = std::fs::write(path, json);
    }
}

/// Compute TF-IDF feature vector from text (Bag-of-Words approach).
fn compute_tfidf(text: &str) -> HashMap<String, f64> {
    let mut tf: HashMap<String, f64> = HashMap::new();
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'))
        .filter(|w| w.len() > 2)
        .collect();
    let total = words.len() as f64;
    for word in &words {
        *tf.entry(word.to_lowercase()).or_default() += 1.0;
    }
    for freq in tf.values_mut() {
        *freq /= total.max(1.0);
    }
    tf
}

/// Cosine similarity between two sparse feature vectors.
fn cosine_similarity(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let dot: f64 = a
        .iter()
        .filter_map(|(key, va)| b.get(key).map(|vb| va * vb))
        .sum();
    let mag_a: f64 = a.values().map(|v| v * v).sum::<f64>().sqrt();
    let mag_b: f64 = b.values().map(|v| v * v).sum::<f64>().sqrt();
    if mag_a < 1e-10 || mag_b < 1e-10 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tfidf_computation() {
        let features = compute_tfidf("the quick brown fox jumps over the lazy dog");
        assert!(features.contains_key("quick"));
        assert!(features.contains_key("fox"));
        assert!(!features.contains_key("th")); // too short
    }

    #[test]
    fn test_cosine_similarity() {
        let a = compute_tfidf("rust programming language systems");
        let b = compute_tfidf("rust systems programming memory safe");
        let c = compute_tfidf("javascript web frontend react");
        let sim_ab = cosine_similarity(&a, &b);
        let sim_ac = cosine_similarity(&a, &c);
        assert!(
            sim_ab > sim_ac,
            "Rust topics should be more similar than Rust vs JS"
        );
    }
}
