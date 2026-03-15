use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::sub_agents::{SubAgentFactory, SubAgentRole};
use crate::tools::{Tool, ToolSecretPolicy};

/// An autonomous sub-agent restricted to browser and web interactions.
/// Defends against prompt injection by isolating the agent from the local filesystem and shell.
pub struct BrowserAgentTool {
    factory: std::sync::Arc<dyn SubAgentFactory>,
}

impl BrowserAgentTool {
    pub fn new(factory: std::sync::Arc<dyn SubAgentFactory>) -> Self {
        Self { factory }
    }
}

#[async_trait]
impl Tool for BrowserAgentTool {
    fn name(&self) -> &str {
        "browser_sub_agent"
    }

    fn description(&self) -> &str {
        "Delegate all web browsing, scraping, and interaction to a specialized browser sub-agent. \
         This agent has access to playwright and browser tools. \
         It does NOT have access to the local filesystem or shell. \
         Use this tool when interacting with untrusted websites to isolate execution."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "instruction": {
                    "type": "string",
                    "description": "Clear instructions for the browser sub-agent on what to navigate to, interact with, or scrape."
                }
            },
            "required": ["instruction"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        // Parallel browsing depends on the underlying browser implementation, but generally we avoid it
        // so they don't fight over the same playwright window context.
        false
    }

    fn secret_policy(&self) -> ToolSecretPolicy {
        // May need credentials for sites
        ToolSecretPolicy::deny()
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let instruction = input
            .get("instruction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'instruction' parameter"))?;

        match self.factory.run_sub_agent(SubAgentRole::Browser, instruction).await {
            Ok(result) => Ok(serde_json::json!({
                "status": "success",
                "sub_agent_response": result
            })),
            Err(e) => Err(anyhow::anyhow!("Browser sub-agent execution failed: {}", e)),
        }
    }
}
