//! # LLM Client
//!
//! Concrete LLM client implementation supporting OpenRouter and Groq APIs.
//! Uses the OpenAI-compatible chat completions API format.
//!
//! API keys are loaded from environment variables or the secure vault.
//! Keys are NEVER included in prompts sent to the model.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::planner::Message;
use crate::llm::LlmProvider;

/// Supported LLM provider backends
#[derive(Debug, Clone)]
pub enum LlmBackend {
    /// OpenRouter API (https://openrouter.ai)
    OpenRouter,
    /// Gyroscape API
    Gyroscape,
    /// Custom API endpoint
    Custom { base_url: String },
}

impl LlmBackend {
    /// Get the base URL for the API
    fn base_url(&self) -> &str {
        match self {
            LlmBackend::OpenRouter => "https://openrouter.ai/api/v1",
            LlmBackend::Gyroscape => "https://api.gyroscape.com/api/v1",
            LlmBackend::Custom { base_url } => base_url,
        }
    }

    /// Get the environment variable name for the API key
    pub fn env_key(&self) -> &str {
        match self {
            LlmBackend::OpenRouter => "OPENROUTER_API_KEY",
            LlmBackend::Gyroscape => "GYROSCAPE_API_KEY",
            LlmBackend::Custom { .. } => "LLM_API_KEY",
        }
    }
}

/// Chat completion request body (OpenAI-compatible format)
#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: serde_json::Value,
}

/// Chat completion response body
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

/// Embedding request body
#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a str,
}

/// Embedding response body
#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// The LLM client sends prompts to an external API.
#[derive(Clone)]
pub struct LlmClient {
    backend: LlmBackend,
    model: String,
    api_key: String,
    client: reqwest::Client,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
}

impl LlmClient {
    /// Create a new LLM client.
    /// The API key is loaded from the environment variable corresponding to the backend.
    pub fn new(backend: LlmBackend, model: &str) -> Result<Self> {
        let api_key = std::env::var(backend.env_key()).with_context(|| {
            format!(
                "Missing API key. Set the {} environment variable.",
                backend.env_key()
            )
        })?;

        Ok(Self {
            backend,
            model: model.to_string(),
            api_key,
            client: reqwest::Client::new(),
            temperature: Some(0.7),
            max_tokens: Some(4096),
        })
    }

    /// Create a client with an explicit API key (e.g. from vault).
    pub fn with_api_key(backend: LlmBackend, model: &str, api_key: String) -> Self {
        Self {
            backend,
            model: model.to_string(),
            api_key,
            client: reqwest::Client::new(),
            temperature: Some(0.7),
            max_tokens: Some(4096),
        }
    }

    /// Set the temperature parameter.
    pub fn set_temperature(&mut self, temp: f64) {
        self.temperature = Some(temp);
    }

    /// Set the max tokens parameter.
    pub fn set_max_tokens(&mut self, tokens: u32) {
        self.max_tokens = Some(tokens);
    }

    /// Send a chat completion request and return the response text.
    pub async fn chat(&self, messages: &[Message]) -> Result<String> {
        let url = format!("{}/chat/completions", self.backend.base_url());

        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let request_body = ChatRequest {
            model: self.model.clone(),
            messages: chat_messages,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to LLM API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error ({}): {}", status, body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse LLM API response")?;

        let content = chat_response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        Ok(content)
    }

    /// Send an embedding request and return the vector.
    pub async fn get_embedding(&self, text: &str) -> Result<Vec<f32>> {
        let mut base = self.backend.base_url().to_string();
        let api_key = self.api_key.clone();

        if base.ends_with("/chat/completions") {
            base = base.replace("/chat/completions", "");
        }

        let url = format!("{}/embeddings", base);

        // Standard embedding model for OpenRouter/Custom, specialized for Gyroscape
        let embed_model = match self.backend {
            LlmBackend::Gyroscape => "embedding-model-small",
            _ => "openai/text-embedding-3-large",
        };

        let request_body = EmbeddingRequest {
            model: embed_model,
            input: text,
        };

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Embeddings API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Embeddings API error ({}): {}", status, body);
        }

        let mut embed_response: EmbeddingResponse = response
            .json()
            .await
            .context("Failed to parse Embeddings API response")?;

        if embed_response.data.is_empty() {
            anyhow::bail!("Embeddings API returned no data array.");
        }

        // Return the first embedding vector
        Ok(embed_response.data.remove(0).embedding)
    }
}

#[async_trait]
impl LlmProvider for LlmClient {
    async fn chat(&self, messages: &[Message]) -> Result<String> {
        self.chat(messages).await
    }

    fn provider_name(&self) -> &str {
        match &self.backend {
            LlmBackend::OpenRouter => "openrouter",
            LlmBackend::Gyroscape => "gyroscape",
            LlmBackend::Custom { .. } => "custom",
        }
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
