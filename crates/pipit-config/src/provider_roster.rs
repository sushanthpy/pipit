//! Provider Roster — multi-provider registry with runtime hot-swap.
//!
//! Auto-discovers available providers from environment variables and
//! credential stores. Each entry is a named profile with everything needed
//! to instantiate a provider: kind, model, API key, optional base URL.
//!
//! The roster supports:
//!   - Auto-discovery: scan env vars + credential store on startup
//!   - Named profiles: "anthropic/opus", "openai/gpt-4o", "ollama/qwen"
//!   - Runtime switching: `/provider` slash command or `/provider <label>`
//!   - Round-robin convenience: `/provider next` cycles through available
//!   - Status bar display: shows active profile label

use crate::ProviderKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single provider profile — everything needed to create a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// Display label (e.g., "anthropic/opus", "openai/gpt-4o")
    pub label: String,
    /// Provider kind
    pub kind: ProviderKind,
    /// Model ID (fully resolved, not an alias)
    pub model: String,
    /// API key (resolved at discovery time)
    pub api_key: String,
    /// Optional base URL override
    pub base_url: Option<String>,
    /// How this profile was discovered
    pub source: ProfileSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileSource {
    /// Auto-discovered from environment variables
    Environment,
    /// Loaded from credential store (~/.pipit/credentials.json)
    CredentialStore,
    /// Explicitly configured (CLI flag or config file)
    Configured,
    /// Added at runtime via /provider add
    Runtime,
}

impl ProfileSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "env",
            Self::CredentialStore => "creds",
            Self::Configured => "config",
            Self::Runtime => "runtime",
        }
    }
}

/// The provider roster — holds all discovered profiles, tracks active.
#[derive(Debug, Clone)]
pub struct ProviderRoster {
    /// All available profiles, keyed by label.
    profiles: Vec<ProviderProfile>,
    /// Index of the currently active profile.
    active_index: usize,
}

impl ProviderRoster {
    /// Build a roster by auto-discovering available providers.
    ///
    /// Scans environment variables and credential store. The `primary` profile
    /// (the one the user explicitly configured or passed via CLI) is always
    /// first and active by default.
    pub fn discover(
        primary_kind: ProviderKind,
        primary_model: &str,
        primary_api_key: &str,
        primary_base_url: Option<&str>,
    ) -> Self {
        let mut profiles = Vec::new();
        let mut seen_labels = std::collections::HashSet::new();

        // Always include the primary (current) provider first
        let primary_label = format_label(primary_kind, primary_model);
        profiles.push(ProviderProfile {
            label: primary_label.clone(),
            kind: primary_kind,
            model: primary_model.to_string(),
            api_key: primary_api_key.to_string(),
            base_url: primary_base_url.map(|s| s.to_string()),
            source: ProfileSource::Configured,
        });
        seen_labels.insert(primary_label);

        // Auto-discover: check each provider for available API keys
        let discovery_targets: Vec<(ProviderKind, &str)> = vec![
            (ProviderKind::Anthropic, "claude-sonnet-4-20250514"),
            (ProviderKind::OpenAi, "gpt-4o"),
            (ProviderKind::DeepSeek, "deepseek-chat"),
            (ProviderKind::Google, "gemini-2.5-pro"),
            (ProviderKind::OpenRouter, "anthropic/claude-sonnet-4"),
            (ProviderKind::XAi, "grok-3"),
            (ProviderKind::Groq, "llama-3.3-70b-versatile"),
            (ProviderKind::Mistral, "mistral-large-latest"),
            (ProviderKind::Cerebras, "llama-3.3-70b"),
            (ProviderKind::Ollama, "qwen2.5-coder:7b"),
            (ProviderKind::GitHubCopilot, "gpt-4o"),
        ];

        for (kind, default_model) in discovery_targets {
            if let Some(key) = crate::resolve_api_key(kind) {
                let label = format_label(kind, default_model);
                if seen_labels.contains(&label) {
                    continue;
                }
                seen_labels.insert(label.clone());
                profiles.push(ProviderProfile {
                    label,
                    kind,
                    model: default_model.to_string(),
                    api_key: key,
                    base_url: None,
                    source: ProfileSource::Environment,
                });
            }
        }

        Self {
            profiles,
            active_index: 0,
        }
    }

    /// Get the active profile.
    pub fn active(&self) -> &ProviderProfile {
        &self.profiles[self.active_index]
    }

    /// Get the active profile index.
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// List all available profiles.
    pub fn list(&self) -> &[ProviderProfile] {
        &self.profiles
    }

    /// Number of profiles.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Whether the roster is empty (should never be — primary is always present).
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Switch to the next profile (round-robin).
    /// Returns the newly active profile.
    pub fn next(&mut self) -> &ProviderProfile {
        self.active_index = (self.active_index + 1) % self.profiles.len();
        &self.profiles[self.active_index]
    }

    /// Switch to the previous profile (round-robin backwards).
    pub fn prev(&mut self) -> &ProviderProfile {
        if self.active_index == 0 {
            self.active_index = self.profiles.len() - 1;
        } else {
            self.active_index -= 1;
        }
        &self.profiles[self.active_index]
    }

    /// Switch to a profile by label (case-insensitive, prefix match).
    /// Returns Ok with the profile, or Err with a "did you mean" message.
    pub fn switch_to(&mut self, query: &str) -> Result<&ProviderProfile, String> {
        let q = query.to_ascii_lowercase();

        // Exact match
        if let Some(idx) = self.profiles.iter().position(|p| p.label.to_ascii_lowercase() == q) {
            self.active_index = idx;
            return Ok(&self.profiles[self.active_index]);
        }

        // Prefix match
        let candidates: Vec<(usize, &ProviderProfile)> = self
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| p.label.to_ascii_lowercase().starts_with(&q))
            .collect();

        match candidates.len() {
            1 => {
                self.active_index = candidates[0].0;
                Ok(&self.profiles[self.active_index])
            }
            0 => {
                // Try matching just the provider name or model name
                if let Some(idx) = self.profiles.iter().position(|p| {
                    let kind_str = format!("{:?}", p.kind).to_ascii_lowercase();
                    kind_str == q || p.model.to_ascii_lowercase().starts_with(&q)
                }) {
                    self.active_index = idx;
                    return Ok(&self.profiles[self.active_index]);
                }

                let available: Vec<&str> = self.profiles.iter().map(|p| p.label.as_str()).collect();
                Err(format!(
                    "No provider matching '{}'. Available: {}",
                    query,
                    available.join(", ")
                ))
            }
            _ => {
                let matches: Vec<&str> = candidates.iter().map(|(_, p)| p.label.as_str()).collect();
                Err(format!(
                    "Ambiguous '{}' — matches: {}. Be more specific.",
                    query,
                    matches.join(", ")
                ))
            }
        }
    }

    /// Switch to a profile by numeric index (1-based for user display).
    pub fn switch_to_index(&mut self, index: usize) -> Result<&ProviderProfile, String> {
        if index == 0 || index > self.profiles.len() {
            return Err(format!(
                "Index {} out of range. Available: 1-{}",
                index,
                self.profiles.len()
            ));
        }
        self.active_index = index - 1;
        Ok(&self.profiles[self.active_index])
    }

    /// Add a profile at runtime.
    pub fn add(&mut self, profile: ProviderProfile) {
        // If a profile with same label exists, replace it
        if let Some(idx) = self.profiles.iter().position(|p| p.label == profile.label) {
            self.profiles[idx] = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// Remove a profile by label. Cannot remove the active profile.
    pub fn remove(&mut self, label: &str) -> Result<(), String> {
        let idx = self
            .profiles
            .iter()
            .position(|p| p.label == label)
            .ok_or_else(|| format!("No profile '{}'", label))?;

        if idx == self.active_index {
            return Err("Cannot remove the active profile. Switch first.".to_string());
        }

        self.profiles.remove(idx);
        // Adjust active_index if needed
        if self.active_index > idx {
            self.active_index -= 1;
        }
        Ok(())
    }

    /// Render a formatted list for display in the TUI.
    pub fn render_list(&self) -> String {
        let mut out = String::new();
        for (i, p) in self.profiles.iter().enumerate() {
            let marker = if i == self.active_index { "▸" } else { " " };
            let source = p.source.label();
            out.push_str(&format!(
                " {} {:2}. {:<30} ({}, {})\n",
                marker,
                i + 1,
                p.label,
                short_model(&p.model),
                source,
            ));
        }
        out
    }

    /// Compact status string for the status bar: "anthropic/opus [1/3]"
    pub fn status_label(&self) -> String {
        let active = &self.profiles[self.active_index];
        if self.profiles.len() <= 1 {
            active.label.clone()
        } else {
            format!(
                "{} [{}/{}]",
                active.label,
                self.active_index + 1,
                self.profiles.len()
            )
        }
    }
}

/// Format a profile label from provider kind and model.
fn format_label(kind: ProviderKind, model: &str) -> String {
    let provider_name = match kind {
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "anthropic",
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible | ProviderKind::OpenAiCodex => {
            "openai"
        }
        ProviderKind::DeepSeek => "deepseek",
        ProviderKind::Google | ProviderKind::GoogleGeminiCli | ProviderKind::GoogleAntigravity => {
            "google"
        }
        ProviderKind::OpenRouter => "openrouter",
        ProviderKind::VercelAiGateway => "vercel",
        ProviderKind::GitHubCopilot => "copilot",
        ProviderKind::XAi => "xai",
        ProviderKind::ZAi => "zai",
        ProviderKind::Cerebras => "cerebras",
        ProviderKind::Groq => "groq",
        ProviderKind::Mistral => "mistral",
        ProviderKind::HuggingFace => "huggingface",
        ProviderKind::MiniMax => "minimax",
        ProviderKind::MiniMaxCn => "minimax-cn",
        ProviderKind::Opencode | ProviderKind::OpencodeGo => "opencode",
        ProviderKind::KimiCoding => "kimi",
        ProviderKind::Ollama => "ollama",
        ProviderKind::AzureOpenAi => "azure",
        ProviderKind::Vertex => "vertex",
        ProviderKind::AmazonBedrock => "bedrock",
    };
    let short = short_model(model);
    format!("{}/{}", provider_name, short)
}

/// Shorten a model ID for display: "claude-sonnet-4-20250514" → "sonnet-4"
fn short_model(model: &str) -> String {
    // Strip common prefixes/suffixes
    let s = model
        .replace("claude-", "")
        .replace("gpt-", "gpt")
        .replace("gemini-", "gemini-")
        .replace("-20250514", "")
        .replace("-20251001", "")
        .replace("-latest", "");
    // Truncate if still long
    if s.len() > 20 {
        format!("{}…", &s[..19])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_label_produces_clean_names() {
        assert_eq!(
            format_label(ProviderKind::Anthropic, "claude-sonnet-4-20250514"),
            "anthropic/sonnet-4"
        );
        assert_eq!(format_label(ProviderKind::OpenAi, "gpt-4o"), "openai/gpt4o");
        assert_eq!(
            format_label(ProviderKind::Ollama, "qwen2.5-coder:7b"),
            "ollama/qwen2.5-coder:7b"
        );
    }

    #[test]
    fn roster_primary_is_always_first() {
        let roster = ProviderRoster::discover(
            ProviderKind::Anthropic,
            "claude-sonnet-4-20250514",
            "sk-test",
            None,
        );
        assert!(!roster.is_empty());
        assert_eq!(roster.active_index(), 0);
        assert_eq!(roster.active().kind, ProviderKind::Anthropic);
    }

    #[test]
    fn next_cycles_through_profiles() {
        let mut roster = ProviderRoster {
            profiles: vec![
                ProviderProfile {
                    label: "a".into(),
                    kind: ProviderKind::Anthropic,
                    model: "m1".into(),
                    api_key: "k1".into(),
                    base_url: None,
                    source: ProfileSource::Configured,
                },
                ProviderProfile {
                    label: "b".into(),
                    kind: ProviderKind::OpenAi,
                    model: "m2".into(),
                    api_key: "k2".into(),
                    base_url: None,
                    source: ProfileSource::Environment,
                },
            ],
            active_index: 0,
        };
        assert_eq!(roster.active().label, "a");
        roster.next();
        assert_eq!(roster.active().label, "b");
        roster.next();
        assert_eq!(roster.active().label, "a"); // wraps
    }

    #[test]
    fn switch_to_by_prefix() {
        let mut roster = ProviderRoster {
            profiles: vec![
                ProviderProfile {
                    label: "anthropic/sonnet-4".into(),
                    kind: ProviderKind::Anthropic,
                    model: "m1".into(),
                    api_key: "k1".into(),
                    base_url: None,
                    source: ProfileSource::Configured,
                },
                ProviderProfile {
                    label: "openai/gpt4o".into(),
                    kind: ProviderKind::OpenAi,
                    model: "m2".into(),
                    api_key: "k2".into(),
                    base_url: None,
                    source: ProfileSource::Environment,
                },
            ],
            active_index: 0,
        };
        assert!(roster.switch_to("openai").is_ok());
        assert_eq!(roster.active().label, "openai/gpt4o");
        assert!(roster.switch_to("ant").is_ok());
        assert_eq!(roster.active().label, "anthropic/sonnet-4");
        assert!(roster.switch_to("nonexistent").is_err());
    }

    #[test]
    fn switch_to_index_works() {
        let mut roster = ProviderRoster {
            profiles: vec![
                ProviderProfile {
                    label: "a".into(),
                    kind: ProviderKind::Anthropic,
                    model: "m1".into(),
                    api_key: "k1".into(),
                    base_url: None,
                    source: ProfileSource::Configured,
                },
                ProviderProfile {
                    label: "b".into(),
                    kind: ProviderKind::OpenAi,
                    model: "m2".into(),
                    api_key: "k2".into(),
                    base_url: None,
                    source: ProfileSource::Environment,
                },
            ],
            active_index: 0,
        };
        assert!(roster.switch_to_index(2).is_ok());
        assert_eq!(roster.active().label, "b");
        assert!(roster.switch_to_index(0).is_err()); // 0 is invalid (1-based)
        assert!(roster.switch_to_index(99).is_err());
    }

    #[test]
    fn status_label_shows_position() {
        let roster = ProviderRoster {
            profiles: vec![
                ProviderProfile {
                    label: "anthropic/sonnet-4".into(),
                    kind: ProviderKind::Anthropic,
                    model: "m".into(),
                    api_key: "k".into(),
                    base_url: None,
                    source: ProfileSource::Configured,
                },
                ProviderProfile {
                    label: "openai/gpt4o".into(),
                    kind: ProviderKind::OpenAi,
                    model: "m".into(),
                    api_key: "k".into(),
                    base_url: None,
                    source: ProfileSource::Environment,
                },
            ],
            active_index: 0,
        };
        assert_eq!(roster.status_label(), "anthropic/sonnet-4 [1/2]");
    }

    #[test]
    fn cannot_remove_active_profile() {
        let mut roster = ProviderRoster {
            profiles: vec![
                ProviderProfile {
                    label: "a".into(),
                    kind: ProviderKind::Anthropic,
                    model: "m".into(),
                    api_key: "k".into(),
                    base_url: None,
                    source: ProfileSource::Configured,
                },
                ProviderProfile {
                    label: "b".into(),
                    kind: ProviderKind::OpenAi,
                    model: "m".into(),
                    api_key: "k".into(),
                    base_url: None,
                    source: ProfileSource::Environment,
                },
            ],
            active_index: 0,
        };
        assert!(roster.remove("a").is_err());
        assert!(roster.remove("b").is_ok());
        assert_eq!(roster.len(), 1);
    }
}
