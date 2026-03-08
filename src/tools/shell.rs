//! # Shell Tool
//!
//! Executes shell commands via `tokio::process::Command`.
//! The command is validated by the executor before reaching this tool.
//! This tool does NOT validate for dangerous commands — that is the executor's job.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use super::Tool;

pub struct ShellTool {
    max_runtime_secs: u64,
}

impl ShellTool {
    pub fn new(max_runtime_secs: u64) -> Self {
        Self { max_runtime_secs }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return the output. \
         Use this for running system commands, scripts, or CLI tools."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "working_directory": {
                    "type": "string",
                    "description": "Optional working directory for the command"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let command = input["command"]
            .as_str()
            .context("Missing 'command' field in input")?;

        let working_dir = input["working_directory"].as_str();

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let timeout_duration = std::time::Duration::from_secs(self.max_runtime_secs);

        let output = match tokio::time::timeout(timeout_duration, cmd.output()).await {
            Ok(output_res) => output_res.context("Failed to execute shell command")?,
            Err(_) => {
                anyhow::bail!(
                    "🛑 SHELL TIMEOUT: Command exceeded maximum allowed runtime of {} seconds.",
                    self.max_runtime_secs
                );
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok(serde_json::json!({
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": stdout,
            "stderr": stderr,
            "success": output.status.success(),
        }))
    }
}
