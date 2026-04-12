use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

/// Top-level configuration struct with all defaults applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipitConfig {
    pub provider: ProviderConfig,
    pub model: ModelConfig,
    pub tools: ToolsConfig,
    pub approval: ApprovalMode,
    pub context: ContextConfig,
    pub pricing: PricingConfig,
    pub extensions: ExtensionsConfig,
    pub ui: UiConfig,
    pub project: ProjectConfig,
}

impl Default for PipitConfig {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            model: ModelConfig::default(),
            tools: ToolsConfig::default(),
            approval: ApprovalMode::AutoEdit,
            context: ContextConfig::default(),
            pricing: PricingConfig::default(),
            extensions: ExtensionsConfig::default(),
            ui: UiConfig::default(),
            project: ProjectConfig::default(),
        }
    }
}

impl PipitConfig {
    pub fn merge(&mut self, layer: PipitConfigLayer) {
        if let Some(p) = layer.provider {
            if let Some(v) = p.default {
                self.provider.default = v;
            }
            if let Some(v) = p.base_url {
                self.provider.custom_base_url = Some(v);
            }
        }
        if let Some(m) = layer.model {
            if let Some(v) = m.default_model {
                self.model.default_model = v;
            }
            if let Some(v) = m.context_window {
                self.model.context_window = v;
            }
            if let Some(v) = m.max_output_tokens {
                self.model.max_output_tokens = v;
            }
        }
        if let Some(a) = layer.approval {
            self.approval = a;
        }
        if let Some(c) = layer.context {
            if let Some(v) = c.max_turns {
                self.context.max_turns = v;
            }
            if let Some(v) = c.output_reserve {
                self.context.output_reserve = v;
            }
            if let Some(v) = c.tool_result_reserve {
                self.context.tool_result_reserve = v;
            }
            if let Some(v) = c.compression_threshold {
                self.context.compression_threshold = v;
            }
            if let Some(v) = c.preserve_recent_messages {
                self.context.preserve_recent_messages = v;
            }
            if let Some(v) = c.max_reflections {
                self.context.max_reflections = v;
            }
        }
        if let Some(p) = layer.pricing {
            for (provider, pricing) in p.providers {
                self.pricing.providers.insert(provider, pricing);
            }
        }
    }
}

/// Partial config for layered merging. Every field is Option.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipitConfigLayer {
    pub provider: Option<ProviderConfigLayer>,
    pub model: Option<ModelConfigLayer>,
    pub approval: Option<ApprovalMode>,
    pub context: Option<ContextConfigLayer>,
    pub pricing: Option<PricingConfigLayer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfigLayer {
    #[serde(alias = "name")]
    pub default: Option<ProviderKind>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfigLayer {
    #[serde(alias = "name")]
    pub default_model: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextConfigLayer {
    pub max_turns: Option<u32>,
    pub output_reserve: Option<u64>,
    pub tool_result_reserve: Option<u64>,
    pub compression_threshold: Option<f64>,
    pub preserve_recent_messages: Option<usize>,
    pub max_reflections: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingConfigLayer {
    #[serde(default)]
    pub providers: HashMap<String, ProviderPricing>,
}

// --- Concrete config types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub default: ProviderKind,
    pub anthropic_base_url: Option<String>,
    pub openai_base_url: Option<String>,
    pub ollama_base_url: Option<String>,
    pub custom_base_url: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            default: ProviderKind::Anthropic,
            anthropic_base_url: None,
            openai_base_url: None,
            ollama_base_url: None,
            custom_base_url: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    AmazonBedrock,
    Anthropic,
    OpenAi,
    /// OpenAI Codex-style provider selection over the standard OpenAI transport.
    OpenAiCodex,
    DeepSeek,
    Google,
    GoogleGeminiCli,
    GoogleAntigravity,
    OpenRouter,
    /// Vercel AI Gateway (OpenAI-compatible transport).
    VercelAiGateway,
    GitHubCopilot,
    XAi,
    ZAi,
    Cerebras,
    Groq,
    Mistral,
    /// Hugging Face router (OpenAI-compatible transport).
    HuggingFace,
    /// MiniMax global endpoint (Anthropic-compatible transport).
    MiniMax,
    /// MiniMax China endpoint (Anthropic-compatible transport).
    MiniMaxCn,
    Opencode,
    OpencodeGo,
    KimiCoding,
    Ollama,
    /// Azure OpenAI endpoint (requires --base-url with resource endpoint)
    AzureOpenAi,
    /// Google Vertex AI (uses OAuth2/gcloud auth instead of API key)
    Vertex,
    /// Generic OpenAI-compatible endpoint (set --base-url)
    OpenAiCompatible,
    /// Generic Anthropic-compatible endpoint (set --base-url)
    AnthropicCompatible,
}

impl FromStr for ProviderKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "amazon_bedrock" | "amazon-bedrock" | "bedrock" => Ok(Self::AmazonBedrock),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "openai" | "open_ai" | "gpt" => Ok(Self::OpenAi),
            "openai_codex" | "openai-codex" | "open_ai_codex" | "cortex" | "codex" => {
                Ok(Self::OpenAiCodex)
            }
            "deepseek" | "deep_seek" => Ok(Self::DeepSeek),
            "google" | "gemini" => Ok(Self::Google),
            "google_gemini_cli" | "google-gemini-cli" | "gemini_cli" | "gemini-cli" => {
                Ok(Self::GoogleGeminiCli)
            }
            "google_antigravity" | "google-antigravity" | "antigravity" => {
                Ok(Self::GoogleAntigravity)
            }
            "vertex" | "vertex_ai" | "vertexai" | "google_vertex" => Ok(Self::Vertex),
            "azure" | "azure_openai" | "azure-openai" | "azureopenai" => Ok(Self::AzureOpenAi),
            "openrouter" | "open_router" => Ok(Self::OpenRouter),
            "vercel_ai_gateway" | "vercel-ai-gateway" | "vercel" | "ai_gateway" => {
                Ok(Self::VercelAiGateway)
            }
            "github_copilot" | "github-copilot" | "git_hub_copilot" | "copilot" => {
                Ok(Self::GitHubCopilot)
            }
            "xai" | "x_ai" | "grok" => Ok(Self::XAi),
            "zai" | "z_ai" => Ok(Self::ZAi),
            "cerebras" => Ok(Self::Cerebras),
            "groq" => Ok(Self::Groq),
            "mistral" => Ok(Self::Mistral),
            "huggingface" | "hugging_face" | "hf" => Ok(Self::HuggingFace),
            "minimax" | "mini_max" => Ok(Self::MiniMax),
            "minimax_cn" | "minimax-cn" | "mini_max_cn" => Ok(Self::MiniMaxCn),
            "opencode" => Ok(Self::Opencode),
            "opencode_go" | "opencode-go" => Ok(Self::OpencodeGo),
            "kimi_coding" | "kimi-coding" => Ok(Self::KimiCoding),
            "ollama" => Ok(Self::Ollama),
            "openai_compatible" | "openai-compatible" | "open_ai_compatible" | "custom" => {
                Ok(Self::OpenAiCompatible)
            }
            "anthropic_compatible" | "anthropic-compatible" => {
                Ok(Self::AnthropicCompatible)
            }
            _ => Err(format!(
                "Unknown provider: {}. Supported: amazon_bedrock, anthropic, openai, openai_codex, azure_openai, deepseek, google, google_gemini_cli, google_antigravity, vertex, openrouter, vercel_ai_gateway, github_copilot, xai, zai, cerebras, groq, mistral, huggingface, minimax, minimax_cn, opencode, opencode_go, kimi_coding, ollama, openai_compatible, anthropic_compatible",
                s
            )),
        }
    }
}

/// Custom Deserialize that routes through FromStr so that user-friendly aliases
/// like `openai_compatible` (instead of serde's `open_ai_compatible`) work in
/// config files.
impl<'de> Deserialize<'de> for ProviderKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<ProviderKind>().map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmazonBedrock => write!(f, "amazon_bedrock"),
            Self::Anthropic => write!(f, "anthropic"),
            Self::OpenAi => write!(f, "openai"),
            Self::OpenAiCodex => write!(f, "openai_codex"),
            Self::DeepSeek => write!(f, "deepseek"),
            Self::Google => write!(f, "google"),
            Self::GoogleGeminiCli => write!(f, "google_gemini_cli"),
            Self::GoogleAntigravity => write!(f, "google_antigravity"),
            Self::Vertex => write!(f, "vertex"),
            Self::AzureOpenAi => write!(f, "azure_openai"),
            Self::OpenRouter => write!(f, "openrouter"),
            Self::VercelAiGateway => write!(f, "vercel_ai_gateway"),
            Self::GitHubCopilot => write!(f, "github_copilot"),
            Self::XAi => write!(f, "xai"),
            Self::ZAi => write!(f, "zai"),
            Self::Cerebras => write!(f, "cerebras"),
            Self::Groq => write!(f, "groq"),
            Self::Mistral => write!(f, "mistral"),
            Self::HuggingFace => write!(f, "huggingface"),
            Self::MiniMax => write!(f, "minimax"),
            Self::MiniMaxCn => write!(f, "minimax_cn"),
            Self::Opencode => write!(f, "opencode"),
            Self::OpencodeGo => write!(f, "opencode_go"),
            Self::KimiCoding => write!(f, "kimi_coding"),
            Self::Ollama => write!(f, "ollama"),
            Self::OpenAiCompatible => write!(f, "openai_compatible"),
            Self::AnthropicCompatible => write!(f, "anthropic_compatible"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderKind;
    use std::str::FromStr;

    #[test]
    fn parses_config_toml_name_alias() {
        use super::{PipitConfigLayer, ProviderKind};

        let toml_str = r#"
[provider]
name = "openai_compatible"
base_url = "http://192.168.1.198:8000"

[model]
name = "qwen"
context_window = 65536
max_output_tokens = 16384
"#;
        let layer: PipitConfigLayer = toml::from_str(toml_str).expect("parse failed");
        let provider = layer.provider.unwrap();
        assert_eq!(provider.default, Some(ProviderKind::OpenAiCompatible));
        assert_eq!(provider.base_url, Some("http://192.168.1.198:8000".into()));

        let model = layer.model.unwrap();
        assert_eq!(model.default_model, Some("qwen".into()));
        assert_eq!(model.context_window, Some(65536));
        assert_eq!(model.max_output_tokens, Some(16384));
    }

    #[test]
    fn parses_new_provider_aliases() {
        assert_eq!(
            ProviderKind::from_str("openai-codex").unwrap(),
            ProviderKind::OpenAiCodex
        );
        assert_eq!(
            ProviderKind::from_str("google-gemini-cli").unwrap(),
            ProviderKind::GoogleGeminiCli
        );
        assert_eq!(
            ProviderKind::from_str("vercel-ai-gateway").unwrap(),
            ProviderKind::VercelAiGateway
        );
        assert_eq!(
            ProviderKind::from_str("huggingface").unwrap(),
            ProviderKind::HuggingFace
        );
        assert_eq!(
            ProviderKind::from_str("github-copilot").unwrap(),
            ProviderKind::GitHubCopilot
        );
        assert_eq!(
            ProviderKind::from_str("minimax-cn").unwrap(),
            ProviderKind::MiniMaxCn
        );
        assert_eq!(
            ProviderKind::from_str("opencode-go").unwrap(),
            ProviderKind::OpencodeGo
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub default_model: String,
    pub context_window: u64,
    pub max_output_tokens: u32,
    pub temperature: f32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default_model: "claude-sonnet-4-20250514".to_string(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            temperature: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    pub max_tool_result_tokens: u64,
    pub shell_timeout_secs: u64,
    pub max_file_size_bytes: u64,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            max_tool_result_tokens: 8192,
            shell_timeout_secs: 300,
            max_file_size_bytes: 1_048_576,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// "Plan" — read-only; every write or command needs explicit approval.
    Suggest,
    /// "Edit with review" — file edits are proposed but each patch needs approval.
    AutoEdit,
    /// "Command with review" — shell commands need approval; safe reads do not.
    CommandReview,
    /// "Full access" — no routine prompts in trusted folders.
    FullAuto,
}

impl ApprovalMode {
    /// Human-readable label shown in the status bar.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Suggest => "Plan",
            Self::AutoEdit => "Edit+review",
            Self::CommandReview => "Cmd+review",
            Self::FullAuto => "Full access",
        }
    }

    /// Cycle to the next mode (for hotkey toggling).
    pub fn next(self) -> Self {
        match self {
            Self::Suggest => Self::AutoEdit,
            Self::AutoEdit => Self::CommandReview,
            Self::CommandReview => Self::FullAuto,
            Self::FullAuto => Self::Suggest,
        }
    }
}

impl std::fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ApprovalMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "suggest" | "plan" => Ok(Self::Suggest),
            "auto_edit" | "autoedit" | "edit" | "edit_review" => Ok(Self::AutoEdit),
            "command_review" | "commandreview" | "cmd" | "cmd_review" => Ok(Self::CommandReview),
            "full_auto" | "fullauto" | "yolo" | "full" | "full_access" => Ok(Self::FullAuto),
            _ => Err(format!(
                "Unknown approval mode: {}. Use: plan, edit, cmd, full",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    pub max_turns: u32,
    pub max_reflections: u32,
    pub output_reserve: u64,
    pub tool_result_reserve: u64,
    pub compression_threshold: f64,
    pub preserve_recent_messages: usize,
    pub model_context_window: u64,
}

/// Trigger context compression at 85% of available history budget.
const DEFAULT_COMPRESSION_THRESHOLD: f64 = 0.85;

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            max_reflections: 3,
            output_reserve: 4096,
            tool_result_reserve: 8192,
            compression_threshold: DEFAULT_COMPRESSION_THRESHOLD,
            preserve_recent_messages: 4,
            model_context_window: 200_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderPricing>,
}

impl PricingConfig {
    pub fn pricing_for(&self, provider_id: &str) -> Option<&ProviderPricing> {
        self.providers.get(provider_id)
    }
}

impl Default for PricingConfig {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
                cache_read_per_million: 0.30,
            },
        );
        providers.insert(
            "openai".to_string(),
            ProviderPricing {
                input_per_million: 2.5,
                output_per_million: 10.0,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "deepseek".to_string(),
            ProviderPricing {
                input_per_million: 0.27,
                output_per_million: 1.10,
                cache_read_per_million: 0.07,
            },
        );
        providers.insert(
            "google".to_string(),
            ProviderPricing {
                input_per_million: 1.25,
                output_per_million: 10.0,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "openrouter".to_string(),
            ProviderPricing {
                input_per_million: 0.0, // varies by model
                output_per_million: 0.0,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "xai".to_string(),
            ProviderPricing {
                input_per_million: 2.0,
                output_per_million: 10.0,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "cerebras".to_string(),
            ProviderPricing {
                input_per_million: 0.10,
                output_per_million: 0.10,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "groq".to_string(),
            ProviderPricing {
                input_per_million: 0.05,
                output_per_million: 0.08,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "mistral".to_string(),
            ProviderPricing {
                input_per_million: 0.25,
                output_per_million: 0.25,
                cache_read_per_million: 0.0,
            },
        );
        providers.insert(
            "ollama".to_string(),
            ProviderPricing {
                input_per_million: 0.0,
                output_per_million: 0.0,
                cache_read_per_million: 0.0,
            },
        );

        Self { providers }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionsConfig {
    pub enabled: bool,
    pub extension_dirs: Vec<String>,
}

impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            extension_dirs: vec![".pipit/extensions".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    pub theme: String,
    pub show_thinking: bool,
    pub show_token_usage: bool,
    pub show_cost: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "default".to_string(),
            show_thinking: true,
            show_token_usage: true,
            show_cost: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub auto_commit: bool,
    pub lint_command: Option<String>,
    pub test_command: Option<String>,
    pub conventions_file: Option<String>,
}

/// CLI overrides — highest priority config source.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub provider: Option<ProviderKind>,
    pub model: Option<String>,
    pub approval_mode: Option<ApprovalMode>,
    pub api_key: Option<String>,
}

/// Retry policy used across providers.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f64,
    pub retryable_statuses: Vec<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            retryable_statuses: vec![429, 500, 502, 503, 529],
        }
    }
}
