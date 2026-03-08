//! # HTTP Tool
//!
//! Makes HTTP requests to external APIs using `reqwest`.
//! Supports GET, POST, PUT, PATCH, DELETE methods.
//! Headers and body are optional.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use super::{Tool, ToolSecretPolicy};

pub struct HttpTool {
    client: reqwest::Client,
}

impl HttpTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Tool for HttpTool {
    fn name(&self) -> &str {
        "http"
    }

    fn description(&self) -> &str {
        "Make HTTP requests to external APIs. \
         Supports GET, POST, PUT, PATCH, DELETE methods with optional headers and body."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                    "description": "HTTP method"
                },
                "url": {
                    "type": "string",
                    "description": "The URL to send the request to"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs",
                    "additionalProperties": { "type": "string" }
                },
                "body": {
                    "description": "Optional request body (JSON)"
                }
            },
            "required": ["method", "url"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        true
    }

    fn secret_policy(&self) -> ToolSecretPolicy {
        ToolSecretPolicy::allow(&["api_key", "auth_token"])
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let method = input["method"].as_str().context("Missing 'method' field")?;
        let url = input["url"].as_str().context("Missing 'url' field")?;

        let mut request = match method.to_uppercase().as_str() {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "PATCH" => self.client.patch(url),
            "DELETE" => self.client.delete(url),
            _ => anyhow::bail!("Unsupported HTTP method: {}", method),
        };

        // Add headers if provided
        if let Some(headers) = input.get("headers").and_then(|h| h.as_object()) {
            for (key, value) in headers {
                if let Some(v) = value.as_str() {
                    request = request.header(key.as_str(), v);
                }
            }
        }

        // Add body if provided
        if let Some(body) = input.get("body") {
            request = request.json(body);
        }

        let response = request.send().await.context("HTTP request failed")?;

        let status = response.status().as_u16();
        let response_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<failed to read body>"));

        // Try to parse body as JSON, fall back to string
        let body_value =
            serde_json::from_str::<Value>(&body_text).unwrap_or(Value::String(body_text));

        Ok(serde_json::json!({
            "status": status,
            "headers": response_headers,
            "body": body_value,
        }))
    }
}
