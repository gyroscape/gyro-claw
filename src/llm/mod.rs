//! # LLM Module
//!
//! Swappable LLM provider architecture.
//! Supports external API backends like OpenRouter.

pub mod client;

use anyhow::Result;
use async_trait::async_trait;

use crate::agent::planner::Message;

/// Trait for swappable LLM providers.
/// Implement this trait to add new LLM backends.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a list of messages and get a response string back.
    async fn chat(&self, messages: &[Message]) -> Result<String>;

    /// Return the provider name (e.g. "openrouter").
    fn provider_name(&self) -> &str;

    /// Return the model name being used.
    fn model_name(&self) -> &str;
}
