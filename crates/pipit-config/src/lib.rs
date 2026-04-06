mod types;
pub mod credentials;
pub mod feature_flags;
pub mod model_routing;

pub use types::*;
pub use credentials::{
    CredentialStore, StoredCredential, OAuthFlow,
    OAuthDeviceConfig, oauth_device_flow, oauth_device_config_for,
};

use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("Config error: {0}")]
    Other(String),
}

/// Resolve configuration from all sources (later wins):
/// 1. Compiled defaults
/// 2. /etc/pipit/config.toml          (system)
/// 3. ~/.config/pipit/config.toml     (user)
/// 4. .pipit/config.toml              (project)
/// 5. PIPIT_* environment variables
/// 6. CLI flags
pub fn resolve_config(
    project_root: Option<&Path>,
    cli_overrides: CliOverrides,
) -> Result<PipitConfig, ConfigError> {
    let mut config = PipitConfig::default();

    // System config
    let system_path = PathBuf::from("/etc/pipit/config.toml");
    if system_path.exists() {
        let layer: PipitConfigLayer = toml::from_str(&std::fs::read_to_string(&system_path)?)?;
        config.merge(layer);
    }

    // User config
    if let Some(config_dir) = dirs::config_dir() {
        let user_path = config_dir.join("pipit").join("config.toml");
        if user_path.exists() {
            let layer: PipitConfigLayer = toml::from_str(&std::fs::read_to_string(&user_path)?)?;
            config.merge(layer);
        }
    }

    // Project config
    if let Some(root) = project_root {
        let project_path = root.join(".pipit").join("config.toml");
        if project_path.exists() {
            let layer: PipitConfigLayer =
                toml::from_str(&std::fs::read_to_string(&project_path)?)?;
            config.merge(layer);
        }
    }

    // Environment variables
    apply_env_overrides(&mut config);

    // CLI overrides (highest priority)
    apply_cli_overrides(&mut config, cli_overrides);

    Ok(config)
}

fn apply_env_overrides(config: &mut PipitConfig) {
    if let Ok(val) = std::env::var("PIPIT_PROVIDER") {
        if let Ok(kind) = val.parse::<ProviderKind>() {
            config.provider.default = kind;
        }
    }
    if let Ok(val) = std::env::var("PIPIT_MODEL") {
        config.model.default_model = val;
    }
    if let Ok(val) = std::env::var("PIPIT_APPROVAL_MODE") {
        if let Ok(mode) = val.parse::<ApprovalMode>() {
            config.approval = mode;
        }
    }
    if let Ok(val) = std::env::var("PIPIT_MAX_TURNS") {
        if let Ok(n) = val.parse::<u32>() {
            config.context.max_turns = n;
        }
    }
    if let Ok(val) = std::env::var("PIPIT_BASE_URL") {
        config.provider.custom_base_url = Some(val);
    }
}

fn apply_cli_overrides(config: &mut PipitConfig, overrides: CliOverrides) {
    if let Some(provider) = overrides.provider {
        config.provider.default = provider;
    }
    if let Some(model) = overrides.model {
        config.model.default_model = model;
    }
    if let Some(mode) = overrides.approval_mode {
        config.approval = mode;
    }
}

/// Detect the project root by walking up from cwd looking for .git or .pipit/
///
/// Note: `~/.pipit` is the global config directory and does NOT count as a
/// project root marker. Only `.pipit` directories outside the home directory
/// are treated as project indicators.
pub fn detect_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let home = dirs::home_dir();
    let mut dir = cwd.as_path();
    loop {
        // .git is always a valid project root marker
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        // .pipit is a project root marker ONLY if this is not the home directory
        // (because ~/.pipit is the global config directory, not a project)
        if dir.join(".pipit").exists() {
            let is_home = home.as_ref().map_or(false, |h| h.as_path() == dir);
            if !is_home {
                return Some(dir.to_path_buf());
            }
        }
        dir = dir.parent()?;
    }
}

/// Resolve API key for a provider. Priority:
/// 1. Environment variable (e.g. `ANTHROPIC_API_KEY`)
/// 2. `~/.pipit/credentials.json` (stored via `pipit auth login`)
/// 3. Google ADC for the `google` provider
pub fn resolve_api_key(provider: ProviderKind) -> Option<String> {
    // 1. Environment variable
    let env_var = match provider {
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "ANTHROPIC_API_KEY",
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => "OPENAI_API_KEY",
        ProviderKind::AzureOpenAi => "AZURE_OPENAI_API_KEY",
        ProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
        ProviderKind::Google => "GOOGLE_API_KEY",
        ProviderKind::Vertex => "VERTEX_API_KEY",
        ProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        ProviderKind::XAi => "XAI_API_KEY",
        ProviderKind::Cerebras => "CEREBRAS_API_KEY",
        ProviderKind::Groq => "GROQ_API_KEY",
        ProviderKind::Mistral => "MISTRAL_API_KEY",
        ProviderKind::Ollama => return Some("ollama".to_string()),
    };
    if let Ok(val) = std::env::var(env_var) {
        return Some(val);
    }

    // 2. Credentials file
    let store = credentials::CredentialStore::load();
    if let Some(token) = store.resolve_token(provider) {
        return Some(token);
    }

    None
}

/// Return the user config directory: `~/.config/pipit/` (follows platform standard).
pub fn user_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("pipit"))
}

/// Return the path for the user config file: `~/.config/pipit/config.toml`.
pub fn user_config_path() -> Option<PathBuf> {
    user_config_dir().map(|d| d.join("config.toml"))
}

/// Check whether a user config file exists.
pub fn has_user_config() -> bool {
    user_config_path().map(|p| p.exists()).unwrap_or(false)
}

/// Write a config layer to the user config file (`~/.config/pipit/config.toml`).
/// Creates the directory if it doesn't exist.
pub fn write_user_config(layer: &PipitConfigLayer) -> Result<(), ConfigError> {
    let dir = user_config_dir().ok_or_else(|| ConfigError::Other("Cannot determine config directory".into()))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let toml_str = toml::to_string_pretty(layer)?;
    std::fs::write(&path, toml_str)?;
    Ok(())
}
