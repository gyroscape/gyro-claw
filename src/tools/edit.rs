//! # File Edit Tool
//!
//! Create, append, replace, insert, and delete lines in files.
//! Blocks editing of system files. Requires confirmation in safe mode.
//! Before applying edits in safe mode, renders a colorized diff.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use console::{style, Term};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use super::Tool;

/// Critical paths that cannot be edited.
const BLOCKED_PATHS: &[&str] = &[
    "/etc",
    "/boot",
    "/dev",
    "/sys",
    "/proc",
    "/root",
    "/usr/bin",
    "/usr/sbin",
    "/sbin",
    "/bin",
];

pub struct EditTool {
    workspace: String,
}

impl EditTool {
    pub fn new(workspace: String) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit files: create, append, replace text, insert lines, or delete lines. \
         Cannot edit system files. Returns the diff of the changes applied."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "action": {
                    "type": "string",
                    "enum": ["create", "append", "replace", "insert", "delete"],
                    "description": "The edit action to perform"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write/append/insert"
                },
                "search": {
                    "type": "string",
                    "description": "Text to search for (used with 'replace' action)"
                },
                "line_number": {
                    "type": "integer",
                    "description": "Line number for insert/delete actions (1-indexed)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "End line number for delete range (inclusive, 1-indexed)"
                }
            },
            "required": ["file", "action"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let file = input["file"].as_str().context("Missing 'file' field")?;
        let action = input["action"].as_str().context("Missing 'action' field")?;

        // Block editing system files
        for blocked in BLOCKED_PATHS {
            if file.starts_with(blocked) {
                bail!("🛑 SANDBOX: Cannot edit system path: {}", file);
            }
        }

        // Canonicalization prevents directory traversal (../../etc/passwd) and Symlink escapes
        if self.workspace != "/" {
            let canonical_workspace = std::fs::canonicalize(&self.workspace)
                .unwrap_or_else(|_| std::path::PathBuf::from(&self.workspace));

            // Try to canonicalize target file. If it doesn't exist yet, canonicalize its parent directory.
            let path_to_check = std::path::Path::new(file);
            let canonical_target = std::fs::canonicalize(path_to_check).unwrap_or_else(|_| {
                path_to_check
                    .parent()
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    .unwrap_or_else(|| std::path::PathBuf::from(file))
            });

            if !canonical_target.starts_with(&canonical_workspace) {
                let err_msg = serde_json::json!({
                    "error_type": "sandbox_violation",
                    "allowed_root": self.workspace,
                    "message": format!("Editing files outside workspace '{}' is not allowed", self.workspace),
                    "suggestion": format!("Use a path starting with {}/", self.workspace)
                });
                anyhow::bail!("{}", err_msg);
            }
        }

        let existing_content = if tokio::fs::try_exists(file).await.unwrap_or(false) {
            tokio::fs::read_to_string(file).await.unwrap_or_default()
        } else {
            String::new()
        };

        let new_content = match action {
            "create" => input["content"].as_str().unwrap_or("").to_string(),
            "append" => {
                let content = input["content"]
                    .as_str()
                    .context("Missing 'content' for append")?;
                let mut existing = existing_content.clone();
                if !existing.ends_with('\n') && !existing.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(content);
                existing
            }
            "replace" => {
                let search = input["search"]
                    .as_str()
                    .context("Missing 'search' for replace")?;
                let content = input["content"]
                    .as_str()
                    .context("Missing 'content' for replace")?;
                if existing_content.matches(search).count() == 0 {
                    return Ok(serde_json::json!({
                        "success": false,
                        "error": format!("Search text not found in {}", file),
                    }));
                }
                existing_content.replace(search, content)
            }
            "insert" => {
                let content = input["content"]
                    .as_str()
                    .context("Missing 'content' for insert")?;
                let line_num = input["line_number"]
                    .as_u64()
                    .context("Missing 'line_number' for insert")?
                    as usize;
                let mut lines: Vec<&str> = existing_content.lines().collect();
                let insert_at = (line_num.saturating_sub(1)).min(lines.len());
                lines.insert(insert_at, content);
                lines.join("\n") + "\n"
            }
            "delete" => {
                let line_num = input["line_number"]
                    .as_u64()
                    .context("Missing 'line_number' for delete")?
                    as usize;
                let end_line = input["end_line"].as_u64().unwrap_or(line_num as u64) as usize;
                let lines: Vec<&str> = existing_content.lines().collect();
                let mut result = Vec::new();
                for (i, line) in lines.iter().enumerate() {
                    let num = i + 1;
                    if num < line_num || num > end_line {
                        result.push(*line);
                    }
                }
                result.join("\n") + "\n"
            }
            _ => bail!("Unknown action: {}", action),
        };

        // Render diff (but we only print it right now since Executor handles confirmation)
        let diff_text = render_diff(&existing_content, &new_content);

        // Try to print the diff directly for CLI UX
        // In autonomous mode it blinks by, in ask mode they see it before typing 'y'
        let term = Term::stderr();
        term.write_line("\n----------------- CODE DIFF PREVIEW -----------------")
            .ok();
        term.write_line(&format!("File: {}", file)).ok();
        term.write_line(&diff_text).ok();
        term.write_line("-----------------------------------------------------")
            .ok();

        // Apply string to file
        if let Some(parent) = std::path::Path::new(file).parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(file, &new_content)
            .await
            .with_context(|| format!("Failed to write to file: {}", file))?;

        Ok(serde_json::json!({
            "success": true,
            "message": format!("Successfully applied '{}' to {}", action, file),
            "diff": diff_text,
        }))
    }
}

/// Helper method to format a string diff into unified terminal styling.
fn render_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            match change.tag() {
                ChangeTag::Delete => {
                    output.push_str(&format!("{} {}", style("-").red(), style(change).red()));
                }
                ChangeTag::Insert => {
                    output.push_str(&format!("{} {}", style("+").green(), style(change).green()));
                }
                ChangeTag::Equal => {
                    // Only show 1 line of context above and below to keep diffs fast to read
                    output.push_str(&format!("  {}", style(change).dim()));
                }
            }
        }
    }
    output
}
