use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::sub_agents::{SubAgentFactory, SubAgentRole};
use crate::tools::{Tool, ToolSecretPolicy};

/// An autonomous sub-agent restricted exclusively to research and reading tools.
/// It cannot run shell commands, edit files, or manipulate the computer.
pub struct ResearcherAgentTool {
    factory: std::sync::Arc<dyn SubAgentFactory>,
}

impl ResearcherAgentTool {
    pub fn new(factory: std::sync::Arc<dyn SubAgentFactory>) -> Self {
        Self { factory }
    }
}

#[async_trait]
impl Tool for ResearcherAgentTool {
    fn name(&self) -> &str {
        "researcher_sub_agent"
    }

    fn description(&self) -> &str {
        "Delegate highly-complex research, code-reading, or search tasks to a dedicated sub-agent. \
         This agent has access to web search, semantic search, project map, and read-only filesystem tools. \
         It does NOT have access to editing files or running shell commands. \
         Use this when you need an autonomous agent to deeply investigate a codebase or topic."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "instruction": {
                    "type": "string",
                    "description": "A highly detailed instruction of what the sub-agent should research, find, or summarize. Provide specific context."
                }
            },
            "required": ["instruction"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        // Technically safe as the researcher only reads data.
        true
    }

    fn secret_policy(&self) -> ToolSecretPolicy {
        // The researcher agent might need api keys to do web searches or semantic searches.
        // It relies on the main agent passing down the environment keys indirectly via factory.
        ToolSecretPolicy::deny()
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let instruction = input
            .get("instruction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'instruction' parameter"))?;

        // Run the sub-agent using the restricted Researcher role.
        match self.factory.run_sub_agent(SubAgentRole::Researcher, instruction).await {
            Ok(result) => Ok(serde_json::json!({
                "status": "success",
                "sub_agent_response": result
            })),
            Err(e) => Err(anyhow::anyhow!("Sub-agent execution failed: {}", e)),
        }
    }
}
