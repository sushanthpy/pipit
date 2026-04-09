use anyhow::Result;
use pipit_config::{ApprovalMode, ProviderKind};

pub fn run() -> Result<()> {
    use std::io::{self, Write};

    let config_path = pipit_config::user_config_path()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;

    println!();
    println!("  \x1b[1;33mpipit setup\x1b[0m");
    println!("  \x1b[90mInteractive configuration wizard\x1b[0m");
    println!();

    if config_path.exists() {
        println!(
            "  \x1b[90mExisting config:\x1b[0m {}",
            config_path.display()
        );
        print!("  Overwrite? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("  Aborted.");
            return Ok(());
        }
        println!();
    }

    // Provider
    println!("  \x1b[1mProvider\x1b[0m");
    println!("  \x1b[90mSupported: amazon_bedrock, anthropic, openai, openai_codex,\x1b[0m");
    println!("  \x1b[90m           azure_openai, deepseek, google, google_gemini_cli,\x1b[0m");
    println!(
        "  \x1b[90m           google_antigravity, vertex, openrouter, vercel_ai_gateway,\x1b[0m"
    );
    println!("  \x1b[90m           github_copilot, xai, zai, cerebras, groq, mistral,\x1b[0m");
    println!(
        "  \x1b[90m           huggingface, minimax, minimax_cn, opencode, opencode_go,\x1b[0m"
    );
    println!(
        "  \x1b[90m           kimi_coding, ollama, openai_compatible, anthropic_compatible\x1b[0m"
    );
    println!();
    let provider_str = prompt_input("  Provider [anthropic]: ")?;
    let provider_str = if provider_str.is_empty() {
        "anthropic".to_string()
    } else {
        provider_str
    };
    let provider_kind: ProviderKind = provider_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;
    println!();

    // Model
    let default_model = default_model_for_provider(provider_kind);
    println!("  \x1b[1mModel\x1b[0m");
    let model_str = prompt_input(&format!("  Model [{}]: ", default_model))?;
    let model = if model_str.is_empty() {
        default_model.to_string()
    } else {
        model_str
    };
    println!();

    // Base URL (for compatible/ollama/custom)
    let base_url = if needs_base_url(provider_kind) {
        let default_url = default_base_url(provider_kind);
        println!("  \x1b[1mBase URL\x1b[0m");
        let url = prompt_input(&format!("  Endpoint URL [{}]: ", default_url))?;
        let url = if url.is_empty() {
            default_url.to_string()
        } else {
            url
        };
        println!();
        Some(url)
    } else {
        None
    };

    // API key
    println!("  \x1b[1mAPI Key\x1b[0m");
    if provider_kind == ProviderKind::Ollama {
        println!("  \x1b[90mOllama doesn't need an API key\x1b[0m");
        println!();
    } else {
        let existing = pipit_config::resolve_api_key(provider_kind);
        if existing.is_some() {
            println!("  \x1b[32m✓ Key already configured\x1b[0m (via env var or credentials)");
            println!();
        } else {
            println!("  \x1b[90mEnter key or leave blank to set later.\x1b[0m");
            println!(
                "  \x1b[90mYou can also use: export {}=<key>\x1b[0m",
                env_var_for_provider(provider_kind)
            );
            let key = prompt_input("  API Key: ")?;
            if !key.is_empty() {
                let mut store = pipit_config::CredentialStore::load();
                store.set(
                    &provider_kind.to_string(),
                    pipit_config::StoredCredential::ApiKey { api_key: key },
                );
                store
                    .save()
                    .map_err(|e| anyhow::anyhow!("Failed to save credentials: {}", e))?;
                println!("  \x1b[32m✓ Key saved to ~/.pipit/credentials.json\x1b[0m");
            }
            println!();
        }
    }

    // Approval mode
    println!("  \x1b[1mApproval Mode\x1b[0m");
    println!("  \x1b[90m  suggest     — read-only, ask before every change\x1b[0m");
    println!("  \x1b[90m  auto_edit   — auto-apply edits, ask for commands\x1b[0m");
    println!("  \x1b[90m  full_auto   — autonomous, no confirmation needed\x1b[0m");
    let approval_str = prompt_input("  Approval mode [full_auto]: ")?;
    let approval_str = if approval_str.is_empty() {
        "full_auto".to_string()
    } else {
        approval_str
    };
    let approval: ApprovalMode = approval_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;
    println!();

    // Build config layer
    let layer = pipit_config::PipitConfigLayer {
        provider: Some(pipit_config::ProviderConfigLayer {
            default: Some(provider_kind),
            base_url: base_url.clone(),
        }),
        model: Some(pipit_config::ModelConfigLayer {
            default_model: Some(model.clone()),
            context_window: None,
            max_output_tokens: None,
        }),
        approval: Some(approval),
        context: None,
        pricing: None,
    };

    pipit_config::write_user_config(&layer)
        .map_err(|e| anyhow::anyhow!("Failed to write config: {}", e))?;

    println!(
        "  \x1b[32m✓ Config saved to {}\x1b[0m",
        config_path.display()
    );
    println!();
    println!("  \x1b[1mSummary\x1b[0m");
    println!("  \x1b[90m  Provider:  \x1b[0m {}", provider_kind);
    println!("  \x1b[90m  Model:     \x1b[0m {}", model);
    if let Some(url) = &base_url {
        println!("  \x1b[90m  Base URL:  \x1b[0m {}", url);
    }
    println!("  \x1b[90m  Approval:  \x1b[0m {}", approval);
    println!();
    println!("  Run \x1b[1mpipit\x1b[0m to start coding!");
    println!();

    Ok(())
}

fn prompt_input(prompt: &str) -> Result<String> {
    use std::io::{self, Write};
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

fn default_model_for_provider(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::AmazonBedrock => "us.anthropic.claude-opus-4-6-v1",
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "claude-sonnet-4-20250514",
        ProviderKind::OpenAi => "gpt-4o",
        ProviderKind::OpenAiCodex => "gpt-5.3-codex",
        ProviderKind::AzureOpenAi => "gpt-4o",
        ProviderKind::DeepSeek => "deepseek-chat",
        ProviderKind::Google => "gemini-2.5-flash",
        ProviderKind::GoogleGeminiCli => "gemini-2.5-pro",
        ProviderKind::GoogleAntigravity => "gemini-3.1-pro-high",
        ProviderKind::Vertex => "gemini-2.5-pro",
        ProviderKind::OpenRouter => "anthropic/claude-sonnet-4-20250514",
        ProviderKind::VercelAiGateway => "anthropic/claude-opus-4-6",
        ProviderKind::GitHubCopilot => "gpt-4o",
        ProviderKind::XAi => "grok-3",
        ProviderKind::ZAi => "glm-5",
        ProviderKind::Cerebras => "llama-4-scout-17b-16e-instruct",
        ProviderKind::Groq => "llama-4-scout-17b-16e-instruct",
        ProviderKind::Mistral => "mistral-large-latest",
        ProviderKind::HuggingFace => "moonshotai/Kimi-K2.5",
        ProviderKind::MiniMax | ProviderKind::MiniMaxCn => "MiniMax-M2.7",
        ProviderKind::Opencode => "claude-opus-4-6",
        ProviderKind::OpencodeGo => "kimi-k2.5",
        ProviderKind::KimiCoding => "kimi-k2-thinking",
        ProviderKind::Ollama => "qwen2.5-coder:14b",
        ProviderKind::OpenAiCompatible => "default",
    }
}

fn needs_base_url(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::AmazonBedrock
            | ProviderKind::GoogleGeminiCli
            | ProviderKind::GoogleAntigravity
            | ProviderKind::OpenAiCompatible
            | ProviderKind::OpenAiCodex
            | ProviderKind::AnthropicCompatible
            | ProviderKind::AzureOpenAi
            | ProviderKind::Vertex
            | ProviderKind::VercelAiGateway
            | ProviderKind::GitHubCopilot
            | ProviderKind::ZAi
            | ProviderKind::HuggingFace
            | ProviderKind::MiniMax
            | ProviderKind::MiniMaxCn
            | ProviderKind::Opencode
            | ProviderKind::OpencodeGo
            | ProviderKind::KimiCoding
            | ProviderKind::Ollama
    )
}

fn default_base_url(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Ollama => "http://localhost:11434",
        ProviderKind::AmazonBedrock => "https://bedrock-runtime.us-east-1.amazonaws.com",
        ProviderKind::AzureOpenAi => "https://your-resource.openai.azure.com",
        ProviderKind::OpenAiCodex => "https://api.openai.com",
        ProviderKind::GoogleGeminiCli => "https://cloudcode-pa.googleapis.com",
        ProviderKind::GoogleAntigravity => "https://daily-cloudcode-pa.sandbox.googleapis.com",
        ProviderKind::VercelAiGateway => "https://ai-gateway.vercel.sh",
        ProviderKind::GitHubCopilot => "https://api.individual.githubcopilot.com",
        ProviderKind::ZAi => "https://api.z.ai/api/coding/paas/v4",
        ProviderKind::HuggingFace => "https://router.huggingface.co",
        ProviderKind::MiniMax => "https://api.minimax.io/anthropic",
        ProviderKind::MiniMaxCn => "https://api.minimaxi.com/anthropic",
        ProviderKind::Opencode => "https://opencode.ai/zen",
        ProviderKind::OpencodeGo => "https://opencode.ai/zen/go/v1",
        ProviderKind::KimiCoding => "https://api.kimi.com/coding",
        _ => "http://localhost:8000",
    }
}

fn env_var_for_provider(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::AmazonBedrock => "AWS_BEARER_TOKEN_BEDROCK",
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "ANTHROPIC_API_KEY",
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible | ProviderKind::OpenAiCodex => {
            "OPENAI_API_KEY"
        }
        ProviderKind::AzureOpenAi => "AZURE_OPENAI_API_KEY",
        ProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
        ProviderKind::Google => "GOOGLE_API_KEY",
        ProviderKind::GoogleGeminiCli => "GOOGLE_GEMINI_CLI_TOKEN",
        ProviderKind::GoogleAntigravity => "GOOGLE_ANTIGRAVITY_TOKEN",
        ProviderKind::Vertex => "VERTEX_API_KEY",
        ProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        ProviderKind::VercelAiGateway => "AI_GATEWAY_API_KEY",
        ProviderKind::GitHubCopilot => "COPILOT_GITHUB_TOKEN",
        ProviderKind::XAi => "XAI_API_KEY",
        ProviderKind::ZAi => "ZAI_API_KEY",
        ProviderKind::Cerebras => "CEREBRAS_API_KEY",
        ProviderKind::Groq => "GROQ_API_KEY",
        ProviderKind::Mistral => "MISTRAL_API_KEY",
        ProviderKind::HuggingFace => "HF_TOKEN",
        ProviderKind::MiniMax => "MINIMAX_API_KEY",
        ProviderKind::MiniMaxCn => "MINIMAX_CN_API_KEY",
        ProviderKind::Opencode | ProviderKind::OpencodeGo => "OPENCODE_API_KEY",
        ProviderKind::KimiCoding => "KIMI_API_KEY",
        ProviderKind::Ollama => "OLLAMA_API_KEY",
    }
}
