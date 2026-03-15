use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::sub_agents::{SubAgentFactory, SubAgentRole};
use crate::tools::{Tool, ToolSecretPolicy};

/// An autonomous sub-agent restricted to editing files and running cargo tests.
/// This agent executes the test-fix loop autonomously.
pub struct CoderAgentTool {
    factory: std::sync::Arc<dyn SubAgentFactory>,
}

impl CoderAgentTool {
    pub fn new(factory: std::sync::Arc<dyn SubAgentFactory>) -> Self {
        Self { factory }
    }
}

#[async_trait]
impl Tool for CoderAgentTool {
    fn name(&self) -> &str {
        "coder_sub_agent"
    }

    fn description(&self) -> &str {
        "Delegate coding tasks, refactoring, project scaffolding, and test execution to a specialized coding sub-agent. \
         This agent has access to edit files, list directories, run the test_runner, and use shell for builds and scaffolding to implement TDD workflows. \
         It focuses on producing complete, production-ready outputs (including required configs and docs) and polished UI where applicable. \
         It does NOT have access to browser automation or computer control. \
         Provide a strict specification of what needs to be coded in the instruction."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "instruction": {
                    "type": "string",
                    "description": "Clear specification of the code to write, files to edit, or tests to run and fix."
                }
            },
            "required": ["instruction"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        // This agent mutates files, so running in parallel with other tools could cause conflicts.
        false
    }

    fn secret_policy(&self) -> ToolSecretPolicy {
        ToolSecretPolicy::deny()
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let instruction = input
            .get("instruction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'instruction' parameter"))?;

        match self.factory.run_sub_agent(SubAgentRole::Coder, instruction).await {
            Ok(result) => Ok(serde_json::json!({
                "status": "success",
                "sub_agent_response": result
            })),
            Err(e) => Err(anyhow::anyhow!("Coder sub-agent execution failed: {}", e)),
        }
    }
}
