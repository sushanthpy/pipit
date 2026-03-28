use anyhow::{Context, Result};
use pipit_config::{
    CredentialStore, OAuthFlow, ProviderKind, StoredCredential,
    oauth_device_config_for, oauth_device_flow,
};

use crate::AuthAction;

pub async fn handle(action: &AuthAction) -> Result<()> {
    match action {
        AuthAction::Login {
            provider,
            api_key,
            device,
            adc,
        } => {
            let provider_kind: ProviderKind = provider
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;
            let mut store = CredentialStore::load();

            if *adc {
                if provider_kind != ProviderKind::Google {
                    anyhow::bail!("--adc is only valid for the google provider");
                }
                eprint!("Verifying Google ADC... ");
                match store.resolve_token(ProviderKind::Google) {
                    Some(_) => {
                        store.set(
                            &provider_kind.to_string(),
                            StoredCredential::GoogleAdc,
                        );
                        store.save().context("Failed to save credentials")?;
                        eprintln!("✓ Google ADC configured");
                        eprintln!(
                            "  Using: gcloud auth application-default print-access-token"
                        );
                    }
                    None => {
                        store.set(
                            &provider_kind.to_string(),
                            StoredCredential::GoogleAdc,
                        );
                        store.save().context("Failed to save credentials")?;
                        eprintln!("⚠ gcloud ADC not available yet");
                        eprintln!("  Run: gcloud auth application-default login");
                        eprintln!("  Marker saved — pipit will retry at runtime.");
                    }
                }
                return Ok(());
            }

            if *device {
                if let Some(config) = oauth_device_config_for(provider_kind) {
                    eprintln!("Starting OAuth device-code flow for {}...", provider);
                    let token = oauth_device_flow(&config)
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;

                    let expires_at = token.expires_in.map(|secs| {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() + secs)
                            .unwrap_or(0)
                    });

                    store.set(
                        &provider_kind.to_string(),
                        StoredCredential::OAuthToken {
                            access_token: token.access_token,
                            refresh_token: token.refresh_token,
                            expires_at,
                            flow: OAuthFlow::DeviceCode,
                        },
                    );
                    store.save().context("Failed to save credentials")?;
                    eprintln!("Credentials saved to ~/.pipit/credentials.json");
                } else {
                    anyhow::bail!(
                        "OAuth device flow not configured for {}. Use --api-key instead.",
                        provider
                    );
                }
                return Ok(());
            }

            // API key flow
            let key = if let Some(k) = api_key {
                k.clone()
            } else {
                eprint!("Enter API key for {}: ", provider);
                let mut input = String::new();
                std::io::stdin()
                    .read_line(&mut input)
                    .context("Failed to read input")?;
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() {
                    anyhow::bail!("No API key provided");
                }
                trimmed
            };

            store.set(
                &provider_kind.to_string(),
                StoredCredential::ApiKey { api_key: key },
            );
            store.save().context("Failed to save credentials")?;
            eprintln!("✓ API key stored for {}", provider);
            if let Some(path) = CredentialStore::path() {
                eprintln!("  Saved to: {}", path.display());
            }
        }

        AuthAction::Logout { provider } => {
            let provider_kind: ProviderKind = provider
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;
            let mut store = CredentialStore::load();
            if store.remove(&provider_kind.to_string()) {
                store.save().context("Failed to save credentials")?;
                eprintln!("✓ Credentials removed for {}", provider);
            } else {
                eprintln!("No credentials found for {}", provider);
            }
        }

        AuthAction::Status => {
            let store = CredentialStore::load();
            let entries = store.list();

            if entries.is_empty() {
                eprintln!("No stored credentials.");
                eprintln!();
                eprintln!("Use `pipit auth login <provider>` to add credentials.");
                eprintln!("Or set environment variables (e.g. OPENAI_API_KEY).");
            } else {
                eprintln!("Stored credentials:");
                eprintln!();
                for (provider, kind) in &entries {
                    let status = match kind {
                        &"api_key" => "API key".to_string(),
                        &"oauth_device" => "OAuth (device flow)".to_string(),
                        &"oauth_code" => "OAuth (auth code)".to_string(),
                        &"google_adc" => {
                            let provider_kind: Result<ProviderKind, _> = provider.parse();
                            if let Ok(pk) = provider_kind {
                                if store.resolve_token(pk).is_some() {
                                    "Google ADC ✓".to_string()
                                } else {
                                    "Google ADC ✗ (run: gcloud auth application-default login)".to_string()
                                }
                            } else {
                                "Google ADC".to_string()
                            }
                        }
                        other => other.to_string(),
                    };
                    eprintln!("  {:20} {}", provider, status);
                }
            }

            eprintln!();
            eprintln!("Environment variables:");
            let env_checks = [
                ("ANTHROPIC_API_KEY", "anthropic"),
                ("OPENAI_API_KEY", "openai"),
                ("DEEPSEEK_API_KEY", "deepseek"),
                ("GOOGLE_API_KEY", "google"),
                ("OPENROUTER_API_KEY", "openrouter"),
                ("XAI_API_KEY", "xai"),
                ("CEREBRAS_API_KEY", "cerebras"),
                ("GROQ_API_KEY", "groq"),
                ("MISTRAL_API_KEY", "mistral"),
            ];
            let mut found_env = false;
            for (var, label) in &env_checks {
                if std::env::var(var).is_ok() {
                    eprintln!("  {:20} {} ✓", label, var);
                    found_env = true;
                }
            }
            if !found_env {
                eprintln!("  (none set)");
            }
        }
    }

    Ok(())
}
