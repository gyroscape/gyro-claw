//! # Web Search Tool
//!
//! Searches DuckDuckGo HTML results and returns normalized structured JSON for agent use.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::{redirect::Policy, Client, Url};
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{info, warn};

use super::Tool;

const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_LIMIT: usize = 10;
const MAX_QUERY_LENGTH: usize = 500;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const MAX_REDIRECTS: usize = 10;
const SEARCH_URL: &str = "https://duckduckgo.com/html/";

#[derive(Debug, Clone, Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

pub struct WebSearchTool {
    client: Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("Gyro-Claw Web Search Agent")
            .redirect(Policy::limited(MAX_REDIRECTS))
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { client }
    }

    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let mut url = Url::parse(SEARCH_URL)?;
        url.query_pairs_mut().append_pair("q", query);

        let html = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        parse_search_results(&html, max_results)
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the internet using DuckDuckGo and return a list of results including title, url, and snippet."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query text"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5, max: 10)",
                    "minimum": 1,
                    "maximum": MAX_RESULTS_LIMIT
                }
            },
            "required": ["query"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let response: Result<Value> = async {
            let query = validate_query(input.get("query").and_then(Value::as_str))?;
            let max_results =
                validate_max_results(input.get("max_results").and_then(Value::as_u64))?;

            info!("web_search query: {}", query);

            let results = self.search(&query, max_results).await?;

            info!("web_search results count: {}", results.len());

            Ok(success_response(results))
        }
        .await;

        Ok(match response {
            Ok(value) => value,
            Err(error) => {
                warn!("web_search failed: {}", error);
                error_response(error.to_string())
            }
        })
    }
}

fn validate_query(query: Option<&str>) -> Result<String> {
    let query = query.ok_or_else(|| anyhow!("Missing 'query' field"))?;
    let trimmed = query.trim();

    if trimmed.is_empty() {
        return Err(anyhow!("Query must not be empty"));
    }

    if trimmed.chars().count() > MAX_QUERY_LENGTH {
        return Err(anyhow!(
            "Query must be at most {} characters",
            MAX_QUERY_LENGTH
        ));
    }

    Ok(trimmed.to_string())
}

fn validate_max_results(max_results: Option<u64>) -> Result<usize> {
    match max_results {
        None => Ok(DEFAULT_MAX_RESULTS),
        Some(0) => Err(anyhow!("max_results must be at least 1")),
        Some(value) if value as usize > MAX_RESULTS_LIMIT => {
            Err(anyhow!("max_results must be at most {}", MAX_RESULTS_LIMIT))
        }
        Some(value) => Ok(value as usize),
    }
}

fn parse_search_results(html: &str, max_results: usize) -> Result<Vec<SearchResult>> {
    let document = Html::parse_document(html);
    let result_selector = parse_selector(".result, .results_links, .result--web", "result")?;
    let title_selector = parse_selector(".result__title a, a.result__a", "title link")?;
    let snippet_selector = parse_selector(".result__snippet", "result snippet")?;

    let mut results = Vec::new();

    for result in document.select(&result_selector) {
        if results.len() >= max_results {
            break;
        }

        let Some(link) = result.select(&title_selector).next() else {
            continue;
        };

        let title = normalize_text(&link.text().collect::<Vec<_>>().join(" "));
        if title.is_empty() {
            continue;
        }

        let Some(url) = extract_result_url(&link) else {
            continue;
        };

        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|node| normalize_text(&node.text().collect::<Vec<_>>().join(" ")))
            .unwrap_or_default();

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    Ok(results)
}

fn parse_selector(selector: &str, label: &str) -> Result<Selector> {
    Selector::parse(selector).map_err(|_| anyhow!("Failed to parse {} selector", label))
}

fn extract_result_url(link: &ElementRef<'_>) -> Option<String> {
    let raw_href = link.value().attr("href")?.trim();
    if raw_href.is_empty() {
        return None;
    }

    normalize_result_url(raw_href)
}

fn normalize_result_url(raw_href: &str) -> Option<String> {
    let normalized = if raw_href.starts_with("//") {
        format!("https:{raw_href}")
    } else if raw_href.starts_with('/') {
        format!("https://duckduckgo.com{raw_href}")
    } else {
        raw_href.to_string()
    };

    let parsed = Url::parse(&normalized).ok()?;

    if parsed.domain() == Some("duckduckgo.com") {
        if let Some((_, target)) = parsed.query_pairs().find(|(key, _)| key == "uddg") {
            let target = target.into_owned();
            let target_url = Url::parse(&target).ok()?;
            return is_safe_result_url(&target_url).then(|| target_url.to_string());
        }
    }

    is_safe_result_url(&parsed).then(|| parsed.to_string())
}

fn is_safe_result_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn success_response(results: Vec<SearchResult>) -> Value {
    json!({
        "status": "ok",
        "results": results,
    })
}

fn error_response(message: String) -> Value {
    json!({
        "status": "error",
        "message": message,
        "results": [],
    })
}

#[cfg(test)]
mod tests {
    use super::{
        error_response, normalize_result_url, parse_search_results, success_response,
        validate_max_results, validate_query,
    };
    use serde_json::json;

    #[test]
    fn validates_query_input() {
        assert!(validate_query(Some("rust ownership")).is_ok());
        assert!(validate_query(Some("   ")).is_err());
        assert!(validate_query(None).is_err());
        assert!(validate_query(Some(&"a".repeat(501))).is_err());
    }

    #[test]
    fn validates_max_results_input() {
        assert_eq!(validate_max_results(None).unwrap(), 5);
        assert_eq!(validate_max_results(Some(3)).unwrap(), 3);
        assert!(validate_max_results(Some(0)).is_err());
        assert!(validate_max_results(Some(11)).is_err());
    }

    #[test]
    fn parses_duckduckgo_html_results() {
        let html = r#"
            <html>
              <body>
                <div class="result">
                  <div class="result__title">
                    <a class="result__a" href="https://www.rust-lang.org/">Rust Programming Language</a>
                  </div>
                  <a class="result__snippet">A language empowering everyone to build reliable and efficient software.</a>
                </div>
                <div class="result">
                  <div class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2F">Tokio</a>
                  </div>
                  <div class="result__snippet">An event-driven, non-blocking I/O platform for Rust.</div>
                </div>
              </body>
            </html>
        "#;

        let results = parse_search_results(html, 5).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "A language empowering everyone to build reliable and efficient software."
        );
        assert_eq!(results[1].url, "https://tokio.rs/");
    }

    #[test]
    fn rejects_unsafe_urls() {
        assert_eq!(
            normalize_result_url("https://example.com/path").as_deref(),
            Some("https://example.com/path")
        );
        assert!(normalize_result_url("javascript:alert(1)").is_none());
        assert!(normalize_result_url("file:///tmp/test.txt").is_none());
    }

    #[test]
    fn returns_strict_response_shapes() {
        assert_eq!(
            success_response(Vec::new()),
            json!({
                "status": "ok",
                "results": [],
            })
        );

        assert_eq!(
            error_response("failed".to_string()),
            json!({
                "status": "error",
                "message": "failed",
                "results": [],
            })
        );
    }
}
