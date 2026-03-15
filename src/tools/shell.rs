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

fn is_long_running_command(command: &str) -> bool {
    let cmd = command.to_lowercase();
    let long_prefixes = [
        "npx create-",
        "npx -y create-",
        "npx create-next-app",
        "npx -y create-next-app",
        "npm install",
        "npm ci",
        "npm run build",
        "npm run test",
        "pnpm install",
        "pnpm i",
        "pnpm build",
        "pnpm test",
        "yarn install",
        "yarn build",
        "yarn test",
        "bun install",
        "bun run build",
        "bun run test",
        "cargo build",
        "cargo test",
        "cargo check",
        "go build",
        "go test",
        "dotnet build",
        "dotnet test",
        "gradle build",
        "gradle test",
        "mvn test",
        "mvn package",
    ];
    long_prefixes.iter().any(|needle| cmd.contains(needle))
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
        let timeout_description = format!(
            "Optional custom timeout in seconds (max {}). For long-running installs/builds, omit this or set it high; the tool may auto-extend for known long commands.",
            self.max_runtime_secs
        );
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
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": timeout_description
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
        
        // Use custom timeout if provided (capped at configured max), otherwise use default max_runtime_secs.
        // For known long-running commands, ignore low timeouts and use the max.
        let mut actual_timeout = self.max_runtime_secs.max(1);
        if let Some(custom_timeout) = input.get("timeout_secs").and_then(Value::as_u64) {
            let desired = custom_timeout.clamp(1, self.max_runtime_secs.max(1));
            actual_timeout = if is_long_running_command(command) {
                self.max_runtime_secs.max(1)
            } else {
                desired
            };
        }

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        } else {
            // Default to writing inside the workspace, not the project root
            cmd.current_dir("./workspace");
        }

        let timeout_duration = std::time::Duration::from_secs(actual_timeout);

        let output = match tokio::time::timeout(timeout_duration, cmd.output()).await {
            Ok(output_res) => output_res.context("Failed to execute shell command")?,
            Err(_) => {
                anyhow::bail!(
                    "🛑 SHELL TIMEOUT: Command exceeded maximum allowed runtime of {} seconds.",
                    actual_timeout
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
