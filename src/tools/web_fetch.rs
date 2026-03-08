//! Fast webpage fetcher for extracting readable content without browser automation.

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use reqwest::{redirect::Policy, Client, Url};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::time::Duration;

use super::Tool;

const DEFAULT_MAX_LENGTH: usize = 5000;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const MAX_REDIRECTS: usize = 10;

pub struct WebFetchTool {
    client: Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("Gyro-Claw Web Fetcher")
            .redirect(Policy::limited(MAX_REDIRECTS))
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a webpage and extract readable text content quickly without using a browser."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP or HTTPS URL to fetch"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum extracted content length in characters (default: 5000)"
                }
            },
            "required": ["url"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let raw_url = input
            .get("url")
            .and_then(|value| value.as_str())
            .context("Missing 'url' field")?;
        let max_length = input
            .get("max_length")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_LENGTH)
            .max(1);

        let url = validate_url(raw_url)?;
        let html = self
            .client
            .get(url.clone())
            .send()
            .await
            .with_context(|| format!("Failed to fetch {}", url))?
            .error_for_status()
            .with_context(|| format!("Web fetch returned error status for {}", url))?
            .text()
            .await
            .with_context(|| format!("Failed to read response body for {}", url))?;

        let (title, content) = extract_readable_content(&html, max_length)?;

        Ok(json!({
            "url": url.as_str(),
            "title": title,
            "content": content.clone(),
            "length": content.chars().count(),
        }))
    }
}

fn validate_url(raw_url: &str) -> Result<Url> {
    let url = Url::parse(raw_url).with_context(|| format!("Invalid URL: {}", raw_url))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => bail!(
            "Unsupported URL scheme '{}'. Only HTTP and HTTPS are allowed.",
            other
        ),
    }
}

fn extract_readable_content(html: &str, max_length: usize) -> Result<(String, String)> {
    let document = Html::parse_document(html);

    let title_selector = parse_selector("title", "title")?;
    let heading_selector = parse_selector("h1, h2", "heading")?;
    let paragraph_selector = parse_selector("article p, main p, p", "paragraph")?;
    let article_selector = parse_selector("article, main, [role='main']", "article")?;
    let body_selector = parse_selector("body", "body")?;

    let title: String = document
        .select(&title_selector)
        .next()
        .map(extract_element_text)
        .unwrap_or_default();

    let mut sections = Vec::new();

    for heading in document.select(&heading_selector) {
        push_unique(&mut sections, extract_element_text(heading));
    }

    for paragraph in document.select(&paragraph_selector) {
        push_unique(&mut sections, extract_element_text(paragraph));
    }

    if sections.is_empty() {
        for article in document.select(&article_selector) {
            push_unique(&mut sections, extract_element_text(article));
        }
    }

    if sections.is_empty() {
        if let Some(body) = document.select(&body_selector).next() {
            push_unique(&mut sections, extract_element_text(body));
        }
    }

    let mut content = sections.join("\n\n");
    if content.is_empty() && !title.is_empty() {
        content = title.clone();
    }

    let truncated: String = content.chars().take(max_length).collect();
    Ok((title, truncated))
}

fn parse_selector(selector: &str, label: &str) -> Result<Selector> {
    Selector::parse(selector).map_err(|_| anyhow!("Failed to parse {} selector", label))
}

fn extract_element_text(element: scraper::ElementRef<'_>) -> String {
    normalize_text(&element.text().collect::<Vec<_>>().join(" "))
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if value.is_empty() || values.iter().any(|existing| existing == &value) {
        return;
    }
    values.push(value);
}

#[cfg(test)]
mod tests {
    use super::{extract_readable_content, validate_url};

    #[test]
    fn accepts_http_and_https_urls() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("http://example.com").is_ok());
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(validate_url("file:///tmp/test.html").is_err());
        assert!(validate_url("ftp://example.com/file.txt").is_err());
        assert!(validate_url("/tmp/test.html").is_err());
    }

    #[test]
    fn extracts_title_headings_and_paragraphs() {
        let html = r#"
            <html>
              <head><title>Rust Lang</title></head>
              <body>
                <nav><a href="/">Home</a></nav>
                <main>
                  <h1>Rust</h1>
                  <p>Fast systems programming language.</p>
                  <p>Memory safety without garbage collection.</p>
                </main>
              </body>
            </html>
        "#;

        let (title, content) =
            extract_readable_content(html, 500).expect("expected extraction to succeed");

        assert_eq!(title, "Rust Lang");
        assert!(content.contains("Rust"));
        assert!(content.contains("Fast systems programming language."));
    }
}
