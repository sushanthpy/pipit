//! GitHub ForgePort Adapter — Concrete implementation using GitHub REST API.
//!
//! Implements ForgePort with: personal access token auth, PR CRUD,
//! review comments, CI status, rate limiting with token bucket.

use async_trait::async_trait;
use pipit_core::integration_ports::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// GitHub REST API v3 adapter implementing ForgePort.
pub struct GitHubForgeAdapter {
    base_url: String,
    token: String,
    owner: String,
    repo: String,
    client: reqwest::Client,
    rate_limiter: TokenBucket,
}

/// Simple token bucket rate limiter for GitHub API (5000 req/hr).
struct TokenBucket {
    tokens: std::sync::Mutex<f64>,
    last_refill: std::sync::Mutex<Instant>,
    rate: f64, // tokens per second
    max_tokens: f64,
}

impl TokenBucket {
    fn new(rate: f64, max_tokens: f64) -> Self {
        Self {
            tokens: std::sync::Mutex::new(max_tokens),
            last_refill: std::sync::Mutex::new(Instant::now()),
            rate,
            max_tokens,
        }
    }

    /// Acquire a token. Returns the wait time if throttled.
    fn acquire(&self) -> std::time::Duration {
        let mut tokens = self.tokens.lock().unwrap();
        let mut last = self.last_refill.lock().unwrap();

        let now = Instant::now();
        let elapsed = now.duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * self.rate).min(self.max_tokens);
        *last = now;

        if *tokens >= 1.0 {
            *tokens -= 1.0;
            std::time::Duration::ZERO
        } else {
            let wait = (1.0 - *tokens) / self.rate;
            *tokens = 0.0;
            std::time::Duration::from_secs_f64(wait)
        }
    }
}

impl GitHubForgeAdapter {
    pub fn new(token: &str, owner: &str, repo: &str) -> Self {
        Self {
            base_url: "https://api.github.com".to_string(),
            token: token.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            client: reqwest::Client::builder()
                .user_agent("pipit-agent")
                .build()
                .unwrap_or_default(),
            // 5000 req/hr = 1.389 req/s, burst of 100
            rate_limiter: TokenBucket::new(5000.0 / 3600.0, 100.0),
        }
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, ForgeError> {
        let wait = self.rate_limiter.acquire();
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        Ok(self
            .client
            .request(method, format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json"))
    }
}

#[async_trait]
impl ForgePort for GitHubForgeAdapter {
    fn name(&self) -> &str {
        "github"
    }

    async fn create_pull_request(&self, spec: PrSpec) -> Result<PrHandle, ForgeError> {
        let body = serde_json::json!({
            "title": spec.title,
            "body": spec.body,
            "head": spec.head_branch,
            "base": spec.base_branch,
            "draft": spec.draft,
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/repos/{}/{}/pulls", self.owner, self.repo),
            )
            .await?
            .json(&body)
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;

        if resp.status() == 422 {
            return Err(ForgeError::Api(
                "PR already exists or validation error".into(),
            ));
        }
        if resp.status() == 401 {
            return Err(ForgeError::Auth("Invalid GitHub token".into()));
        }
        if resp.status() == 403 {
            return Err(ForgeError::RateLimited {
                retry_after_secs: 60,
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ForgeError::Api(e.to_string()))?;

        Ok(PrHandle {
            number: json["number"].as_u64().unwrap_or(0),
            url: json["url"].as_str().unwrap_or("").to_string(),
            html_url: json["html_url"].as_str().unwrap_or("").to_string(),
            state: json["state"].as_str().unwrap_or("open").to_string(),
        })
    }

    async fn list_review_comments(&self, pr: &PrHandle) -> Result<Vec<ReviewComment>, ForgeError> {
        let resp = self
            .request(
                reqwest::Method::GET,
                &format!(
                    "/repos/{}/{}/pulls/{}/comments",
                    self.owner, self.repo, pr.number
                ),
            )
            .await?
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;

        let json: Vec<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| ForgeError::Api(e.to_string()))?;

        Ok(json
            .iter()
            .map(|c| ReviewComment {
                id: c["id"].as_u64().unwrap_or(0),
                body: c["body"].as_str().unwrap_or("").to_string(),
                path: c["path"].as_str().unwrap_or("").to_string(),
                line: c["line"].as_u64().map(|l| l as u32),
                author: c["user"]["login"].as_str().unwrap_or("").to_string(),
                created_at: c["created_at"].as_str().unwrap_or("").to_string(),
            })
            .collect())
    }

    async fn post_review_comment(
        &self,
        pr: &PrHandle,
        comment: ReviewCommentSpec,
    ) -> Result<(), ForgeError> {
        let mut body = serde_json::json!({
            "body": comment.body,
            "path": comment.path,
            "commit_id": "HEAD",
        });
        if let Some(line) = comment.line {
            body["line"] = serde_json::json!(line);
        }

        self.request(
            reqwest::Method::POST,
            &format!(
                "/repos/{}/{}/pulls/{}/comments",
                self.owner, self.repo, pr.number
            ),
        )
        .await?
        .json(&body)
        .send()
        .await
        .map_err(|e| ForgeError::Network(e.to_string()))?;
        Ok(())
    }

    async fn ci_status(&self, commit_sha: &str) -> Result<CiStatus, ForgeError> {
        let resp = self
            .request(
                reqwest::Method::GET,
                &format!(
                    "/repos/{}/{}/commits/{}/check-runs",
                    self.owner, self.repo, commit_sha
                ),
            )
            .await?
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ForgeError::Api(e.to_string()))?;

        let checks: Vec<CiCheck> = json["check_runs"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|c| CiCheck {
                name: c["name"].as_str().unwrap_or("").to_string(),
                status: match c["conclusion"].as_str().unwrap_or("pending") {
                    "success" => CiState::Success,
                    "failure" => CiState::Failure,
                    "error" | "cancelled" | "timed_out" => CiState::Error,
                    _ => CiState::Pending,
                },
                url: c["html_url"].as_str().map(String::from),
            })
            .collect();

        let state = if checks.iter().all(|c| c.status == CiState::Success) {
            CiState::Success
        } else if checks.iter().any(|c| c.status == CiState::Failure) {
            CiState::Failure
        } else if checks.iter().any(|c| c.status == CiState::Error) {
            CiState::Error
        } else {
            CiState::Pending
        };

        Ok(CiStatus { state, checks })
    }

    async fn create_issue(&self, spec: IssueSpec) -> Result<IssueHandle, ForgeError> {
        let body = serde_json::json!({
            "title": spec.title,
            "body": spec.body,
            "labels": spec.labels,
            "assignees": spec.assignees,
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/repos/{}/{}/issues", self.owner, self.repo),
            )
            .await?
            .json(&body)
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ForgeError::Api(e.to_string()))?;

        Ok(IssueHandle {
            number: json["number"].as_u64().unwrap_or(0),
            url: json["html_url"].as_str().unwrap_or("").to_string(),
        })
    }

    async fn install_app(&self, _org: &str) -> Result<InstallationToken, ForgeError> {
        // GitHub App OAuth device flow (RFC 8628)
        // In production, this would POST to /login/device/code and poll
        Err(ForgeError::Api(
            "GitHub App installation requires interactive OAuth flow".into(),
        ))
    }

    async fn rate_limit_status(&self) -> Result<RateLimitInfo, ForgeError> {
        let resp = self
            .request(reqwest::Method::GET, "/rate_limit")
            .await?
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ForgeError::Api(e.to_string()))?;

        Ok(RateLimitInfo {
            remaining: json["rate"]["remaining"].as_u64().unwrap_or(0) as u32,
            limit: json["rate"]["limit"].as_u64().unwrap_or(5000) as u32,
            reset_at: json["rate"]["reset"].as_u64().unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_allows_under_rate() {
        let tb = TokenBucket::new(10.0, 10.0);
        let wait = tb.acquire();
        assert_eq!(wait, std::time::Duration::ZERO);
    }

    #[test]
    fn token_bucket_throttles_at_capacity() {
        let tb = TokenBucket::new(1.0, 2.0);
        tb.acquire(); // 1 remaining
        tb.acquire(); // 0 remaining
        let wait = tb.acquire(); // should wait
        assert!(wait > std::time::Duration::ZERO);
    }
}
