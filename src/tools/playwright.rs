use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

pub struct PlaywrightTool {
    client: Client,
    endpoint: String,
}

impl PlaywrightTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            endpoint: std::env::var("GYRO_CLAW_PLAYWRIGHT_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:4000/playwright/action".to_string()),
        }
    }
}

#[async_trait]
impl Tool for PlaywrightTool {
    fn name(&self) -> &str {
        "playwright"
    }

    fn description(&self) -> &str {
        "Automate web browsers reliably using an external Playwright server.
        Use this tool INSTEAD of 'mouse', 'keyboard', or 'browser' when the goal involves interacting with websites.
        The Playwright service must be running and reachable over HTTP.
        Actions:
        - 'open_url': (requires 'url') Navigate to a web page.
        - 'search': (requires 'query', optionally 'selector') Types query into search box and presses Enter.
        - 'click': (requires 'selector') Clicks a DOM element.
        - 'type': (requires 'selector', 'text') Types text into a DOM element.
        - 'press': (requires 'key' or 'text', optionally 'selector') Presses a key, optionally after focusing an element.
        - 'screenshot': Captures the current page state.
        - 'extract': (optionally 'selector', defaults to 'p') Extracts trimmed innerText from all matching elements.
        - 'extract_text': Extracts the full readable page text, capped for prompt safety.
        PRIORITIZE THIS TOOL over manual mouse/keyboard automation for website navigation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The Playwright action to perform.",
                    "enum": ["open_url", "search", "click", "type", "press", "screenshot", "extract", "extract_text"]
                },
                "url": {
                    "type": "string",
                    "description": "The URL to navigate to (required for 'open_url')."
                },
                "query": {
                    "type": "string",
                    "description": "The search query (required for 'search')."
                },
                "selector": {
                    "type": "string",
                    "description": "The CSS selector to target (required for 'click', 'type', optionally 'search' and 'extract')."
                },
                "text": {
                    "type": "string",
                    "description": "The text to type (required for 'type')."
                },
                "key": {
                    "type": "string",
                    "description": "The keyboard key to press (required for 'press' if 'text' is not provided)."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .context("Missing action in input payload")?;

        tracing::info!("Executing action: playwright {}", action);

        let response = match self.client.post(&self.endpoint).json(&input).send().await {
            Ok(response) => response,
            Err(err) => {
                return Ok(tool_error(
                    "playwright",
                    "service_unavailable",
                    format!(
                        "Failed to connect to Playwright server at {}: {}",
                        self.endpoint, err
                    ),
                    "Start the Playwright server or set GYRO_CLAW_PLAYWRIGHT_ENDPOINT. Fall back to local browser/computer tools for this run.",
                ));
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error body".to_string());

            if let Ok(parsed_error) = serde_json::from_str::<Value>(&error_text) {
                if parsed_error.get("status").and_then(|v| v.as_str()) == Some("error") {
                    return Ok(json!({
                        "status": "error",
                        "tool": "playwright",
                        "error_type": parsed_error.get("type").and_then(|v| v.as_str()).unwrap_or("server_error"),
                        "message": parsed_error.get("message").and_then(|v| v.as_str()).unwrap_or("Playwright server request failed"),
                        "suggestion": parsed_error.get("action").and_then(|v| v.as_str()).unwrap_or("Inspect the Playwright server logs and retry."),
                        "http_status": status.as_u16()
                    }));
                }
            }

            return Ok(tool_error(
                "playwright",
                "server_error",
                format!("Playwright server error ({}): {}", status, error_text),
                "Check the Playwright service logs and request payload.",
            ));
        }

        let json_result: Value = match response.json().await {
            Ok(value) => value,
            Err(err) => {
                return Ok(tool_error(
                    "playwright",
                    "invalid_response",
                    format!("Failed to parse Playwright server JSON response: {}", err),
                    "Fix the Playwright server response format or inspect its logs.",
                ));
            }
        };

        Ok(json_result)
    }
}

fn tool_error(tool: &str, error_type: &str, message: String, suggestion: &str) -> Value {
    json!({
        "status": "error",
        "tool": tool,
        "error_type": error_type,
        "message": message,
        "suggestion": suggestion
    })
}
