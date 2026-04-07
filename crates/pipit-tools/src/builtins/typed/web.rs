//! Typed Web Search — multi-provider web search with real API dispatch.
//!
//! Supports Brave Search, Tavily, and SerpAPI. Provider is selected by
//! checking environment variables in priority order:
//!   1. BRAVE_SEARCH_API_KEY → Brave Search API
//!   2. TAVILY_API_KEY → Tavily Search API
//!   3. SERPAPI_API_KEY → SerpAPI
//!
//! Falls back to DuckDuckGo HTML scraping if no API key is configured.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

/// Web search input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebSearchInput {
    /// Search query.
    pub query: String,
    /// Maximum number of results (default: 5).
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

fn default_max_results() -> u32 { 5 }

/// Multi-provider web search tool.
pub struct TypedWebSearchTool;

#[async_trait]
impl TypedTool for TypedWebSearchTool {
    type Input = WebSearchInput;
    const NAME: &'static str = "web_search_typed";
    const CAPABILITIES: CapabilitySet = CapabilitySet::NETWORK_READ;
    const PURITY: Purity = Purity::Idempotent;

    fn describe() -> ToolCard {
        ToolCard {
            name: "web_search_typed".into(),
            summary: "Search the web using configured search provider".into(),
            when_to_use: "When you need current information from the web, such as documentation, API references, or recent developments.".into(),
            examples: vec![
                ToolExample {
                    description: "Search for docs".into(),
                    input: serde_json::json!({
                        "query": "rust tokio spawn_blocking documentation",
                        "max_results": 5
                    }),
                },
            ],
            tags: vec!["web".into(), "search".into(), "internet".into(), "documentation".into()],
            purity: Purity::Idempotent,
            capabilities: CapabilitySet::NETWORK_READ.0,
        }
    }

    async fn execute(
        &self,
        input: WebSearchInput,
        _ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("pipit-cli/0.2.3")
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client error: {e}")))?;

        // Try providers in priority order
        if let Ok(key) = std::env::var("BRAVE_SEARCH_API_KEY") {
            return brave_search(&client, &key, &input.query, input.max_results, cancel).await;
        }
        if let Ok(key) = std::env::var("TAVILY_API_KEY") {
            return tavily_search(&client, &key, &input.query, input.max_results, cancel).await;
        }
        if let Ok(key) = std::env::var("SERPAPI_API_KEY") {
            return serpapi_search(&client, &key, &input.query, input.max_results, cancel).await;
        }

        // Fallback: DuckDuckGo HTML lite (no API key required)
        ddg_search(&client, &input.query, input.max_results, cancel).await
    }
}

/// Brave Search API — https://api.search.brave.com/
async fn brave_search(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
    max_results: u32,
    cancel: CancellationToken,
) -> Result<TypedToolResult, ToolError> {
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding::encode(query),
        max_results,
    );

    let response = tokio::select! {
        r = client.get(&url).header("X-Subscription-Token", api_key).header("Accept", "application/json").send() => {
            r.map_err(|e| ToolError::ExecutionFailed(format!("Brave search failed: {e}")))?
        }
        _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!("Brave API error {status}: {body}")));
    }

    let json: serde_json::Value = response.json().await
        .map_err(|e| ToolError::ExecutionFailed(format!("Brave JSON parse error: {e}")))?;

    let mut results = Vec::new();
    if let Some(web) = json.get("web").and_then(|w| w.get("results")).and_then(|r| r.as_array()) {
        for item in web.iter().take(max_results as usize) {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let desc = item.get("description").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!("**{}**\n{}\n{}\n", title, url, desc));
        }
    }

    Ok(TypedToolResult::text(if results.is_empty() {
        format!("No results found for '{query}' (Brave Search)")
    } else {
        format!("Web search results for '{}' ({} results, Brave):\n\n{}", query, results.len(), results.join("\n"))
    }))
}

/// Tavily Search API — https://api.tavily.com/
async fn tavily_search(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
    max_results: u32,
    cancel: CancellationToken,
) -> Result<TypedToolResult, ToolError> {
    let body = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "max_results": max_results,
        "search_depth": "basic",
    });

    let response = tokio::select! {
        r = client.post("https://api.tavily.com/search").json(&body).send() => {
            r.map_err(|e| ToolError::ExecutionFailed(format!("Tavily search failed: {e}")))?
        }
        _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
    };

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!("Tavily API error: {body}")));
    }

    let json: serde_json::Value = response.json().await
        .map_err(|e| ToolError::ExecutionFailed(format!("Tavily JSON parse error: {e}")))?;

    let mut results = Vec::new();
    if let Some(items) = json.get("results").and_then(|r| r.as_array()) {
        for item in items.iter().take(max_results as usize) {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!("**{}**\n{}\n{}\n", title, url, content));
        }
    }

    Ok(TypedToolResult::text(if results.is_empty() {
        format!("No results found for '{query}' (Tavily)")
    } else {
        format!("Web search results for '{}' ({} results, Tavily):\n\n{}", query, results.len(), results.join("\n"))
    }))
}

/// SerpAPI — https://serpapi.com/
async fn serpapi_search(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
    max_results: u32,
    cancel: CancellationToken,
) -> Result<TypedToolResult, ToolError> {
    let url = format!(
        "https://serpapi.com/search.json?q={}&api_key={}&num={}",
        urlencoding::encode(query), api_key, max_results,
    );

    let response = tokio::select! {
        r = client.get(&url).send() => {
            r.map_err(|e| ToolError::ExecutionFailed(format!("SerpAPI search failed: {e}")))?
        }
        _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
    };

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!("SerpAPI error: {body}")));
    }

    let json: serde_json::Value = response.json().await
        .map_err(|e| ToolError::ExecutionFailed(format!("SerpAPI JSON parse error: {e}")))?;

    let mut results = Vec::new();
    if let Some(organic) = json.get("organic_results").and_then(|r| r.as_array()) {
        for item in organic.iter().take(max_results as usize) {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let link = item.get("link").and_then(|v| v.as_str()).unwrap_or("");
            let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!("**{}**\n{}\n{}\n", title, link, snippet));
        }
    }

    Ok(TypedToolResult::text(if results.is_empty() {
        format!("No results found for '{query}' (SerpAPI)")
    } else {
        format!("Web search results for '{}' ({} results, SerpAPI):\n\n{}", query, results.len(), results.join("\n"))
    }))
}

/// DuckDuckGo HTML lite fallback — no API key required.
/// Scrapes the lite HTML search page.
async fn ddg_search(
    client: &reqwest::Client,
    query: &str,
    max_results: u32,
    cancel: CancellationToken,
) -> Result<TypedToolResult, ToolError> {
    let url = format!(
        "https://lite.duckduckgo.com/lite/?q={}",
        urlencoding::encode(query),
    );

    let response = tokio::select! {
        r = client.get(&url).send() => {
            r.map_err(|e| ToolError::ExecutionFailed(format!("DuckDuckGo search failed: {e}")))?
        }
        _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
    };

    let html = response.text().await
        .map_err(|e| ToolError::ExecutionFailed(format!("DuckDuckGo read error: {e}")))?;

    // Parse result links from lite HTML
    let mut results = Vec::new();
    for line in html.lines() {
        let trimmed = line.trim();
        // DuckDuckGo lite wraps results in <a> tags with class="result-link"
        if trimmed.contains("result-link") || (trimmed.starts_with("<a") && trimmed.contains("href=\"http")) {
            // Extract href
            if let Some(href_start) = trimmed.find("href=\"") {
                let rest = &trimmed[href_start + 6..];
                if let Some(href_end) = rest.find('"') {
                    let url = &rest[..href_end];
                    // Extract link text
                    let text = rest.find('>').and_then(|s| rest[s+1..].find('<').map(|e| &rest[s+1..s+1+e])).unwrap_or("");
                    if !url.is_empty() && url.starts_with("http") {
                        results.push(format!("**{}**\n{}\n", text, url));
                    }
                }
            }
        }
        if results.len() >= max_results as usize {
            break;
        }
    }

    Ok(TypedToolResult::text(if results.is_empty() {
        format!("No results found for '{query}' (DuckDuckGo)")
    } else {
        format!(
            "Web search results for '{}' ({} results, DuckDuckGo — set BRAVE_SEARCH_API_KEY or TAVILY_API_KEY for better results):\n\n{}",
            query, results.len(), results.join("\n")
        )
    }))
}
