//! # Git Tool
//!
//! Execute git operations: status, diff, log, commit, add, branch.
//! Uses `std::process::Command` internally.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use super::Tool;

pub struct GitTool;

impl GitTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Execute git operations: status, diff, log, commit, add, branch. \
         Useful for version control tasks."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "commit", "add", "branch", "checkout"],
                    "description": "The git command to run"
                },
                "args": {
                    "type": "string",
                    "description": "Additional arguments (e.g. file path, commit message, branch name)"
                },
                "working_directory": {
                    "type": "string",
                    "description": "Repository directory (default: current directory)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let git_cmd = input["command"]
            .as_str()
            .context("Missing 'command' field")?;
        let args = input["args"].as_str().unwrap_or("");
        let working_dir = input["working_directory"].as_str().unwrap_or(".");

        // Build the git command
        let mut cmd = Command::new("git");
        cmd.current_dir(working_dir);

        match git_cmd {
            "status" => {
                cmd.arg("status").arg("--short");
            }
            "diff" => {
                cmd.arg("diff");
                if !args.is_empty() {
                    cmd.arg(args);
                }
            }
            "log" => {
                cmd.arg("log")
                    .arg("--oneline")
                    .arg("-n")
                    .arg(if args.is_empty() { "10" } else { args });
            }
            "commit" => {
                if args.is_empty() {
                    return Ok(serde_json::json!({
                        "success": false,
                        "error": "Commit message is required. Pass it in 'args'.",
                    }));
                }
                cmd.arg("commit").arg("-m").arg(args);
            }
            "add" => {
                cmd.arg("add");
                if args.is_empty() {
                    cmd.arg(".");
                } else {
                    cmd.arg(args);
                }
            }
            "branch" => {
                cmd.arg("branch");
                if !args.is_empty() {
                    cmd.arg(args);
                }
            }
            "checkout" => {
                if args.is_empty() {
                    return Ok(serde_json::json!({
                        "success": false,
                        "error": "Branch name is required. Pass it in 'args'.",
                    }));
                }
                cmd.arg("checkout").arg(args);
            }
            _ => {
                return Ok(serde_json::json!({
                    "success": false,
                    "error": format!("Unknown git command: {}", git_cmd),
                }));
            }
        }

        let output = cmd
            .output()
            .await
            .context("Failed to execute git command")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok(serde_json::json!({
            "success": output.status.success(),
            "command": format!("git {}", git_cmd),
            "stdout": stdout,
            "stderr": stderr,
        }))
    }
}
