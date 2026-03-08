//! # Test Runner Tool
//!
//! Run cargo test, cargo build, and cargo check.
//! Captures stdout/stderr and returns results.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use super::Tool;

pub struct TestRunnerTool;

impl TestRunnerTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for TestRunnerTool {
    fn name(&self) -> &str {
        "test_runner"
    }

    fn description(&self) -> &str {
        "Run Rust project commands: cargo test, cargo build, cargo check, cargo clippy. \
         Returns compilation output and test results."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["test", "build", "check", "clippy"],
                    "description": "The cargo command to run"
                },
                "args": {
                    "type": "string",
                    "description": "Additional arguments (e.g. test name, --release)"
                },
                "working_directory": {
                    "type": "string",
                    "description": "Project directory (default: current directory)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let cargo_cmd = input["command"]
            .as_str()
            .context("Missing 'command' field")?;
        let args = input["args"].as_str().unwrap_or("");
        let working_dir = input["working_directory"].as_str().unwrap_or(".");

        let mut cmd = Command::new("cargo");
        cmd.current_dir(working_dir);
        cmd.arg(cargo_cmd);

        // Add extra args if provided
        if !args.is_empty() {
            for arg in args.split_whitespace() {
                cmd.arg(arg);
            }
        }

        let output = cmd
            .output()
            .await
            .context("Failed to execute cargo command")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Truncate very long output
        let max_len = 4000;
        let stdout_truncated = if stdout.len() > max_len {
            format!("{}...\n[truncated]", &stdout[..max_len])
        } else {
            stdout
        };
        let stderr_truncated = if stderr.len() > max_len {
            format!("{}...\n[truncated]", &stderr[..max_len])
        } else {
            stderr
        };

        Ok(serde_json::json!({
            "success": output.status.success(),
            "command": format!("cargo {}", cargo_cmd),
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": stdout_truncated,
            "stderr": stderr_truncated,
        }))
    }
}
