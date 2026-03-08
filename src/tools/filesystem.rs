//! # Filesystem Tool
//!
//! Read, write, append, delete, and list files on the local filesystem.

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use super::Tool;

const CRITICAL_PATH_PREFIXES: [&str; 8] = [
    "/etc", "/boot", "/dev", "/sys", "/proc", "/root", "/usr/bin", "/sbin",
];

pub struct FilesystemTool {
    workspace: String,
}

impl FilesystemTool {
    pub fn new(workspace: String) -> Self {
        Self { workspace }
    }

    fn success_response(action: &str, path: &str, extra_data: Value) -> Value {
        let mut data = serde_json::Map::new();
        data.insert("action".to_string(), Value::String(action.to_string()));
        data.insert("path".to_string(), Value::String(path.to_string()));

        if let Value::Object(map) = extra_data {
            for (key, value) in map {
                data.insert(key, value);
            }
        }

        json!({
            "status": "ok",
            "data": Value::Object(data),
        })
    }

    fn error_response(message: impl Into<String>) -> Value {
        json!({
            "status": "error",
            "message": message.into(),
        })
    }

    fn require_action(input: &Value) -> Result<String> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing 'action' parameter"))?
            .trim()
            .to_lowercase();

        if action.is_empty() {
            bail!("missing 'action' parameter");
        }

        Ok(action)
    }

    fn require_path(input: &Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing 'path' parameter"))?
            .trim()
            .to_string();

        if path.is_empty() {
            bail!("missing 'path' parameter");
        }

        Ok(path)
    }

    fn require_content(input: &Value, action: &str) -> Result<String> {
        let content = input
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing 'content' parameter for {} action", action))?;

        Ok(content.to_string())
    }

    fn resolve_path(&self, raw_path: &str) -> PathBuf {
        let provided = PathBuf::from(raw_path);
        if provided.is_absolute() {
            return provided;
        }

        let workspace = PathBuf::from(&self.workspace);
        if Self::is_workspace_prefixed(&provided, &workspace) {
            provided
        } else {
            workspace.join(provided)
        }
    }

    fn enforce_path_safety(&self, target_path: &Path) -> Result<()> {
        let workspace_path = Path::new(&self.workspace);
        let absolute_workspace = Self::absolutize_path(workspace_path);
        let canonical_workspace =
            std::fs::canonicalize(&absolute_workspace).unwrap_or(absolute_workspace);

        let absolute_target = Self::absolutize_path(target_path);
        let canonical_target = std::fs::canonicalize(&absolute_target).unwrap_or_else(|_| {
            absolute_target
                .parent()
                .and_then(|parent| std::fs::canonicalize(parent).ok())
                .map(|parent| {
                    if let Some(name) = absolute_target.file_name() {
                        parent.join(name)
                    } else {
                        parent
                    }
                })
                .unwrap_or(absolute_target)
        });

        for critical in &CRITICAL_PATH_PREFIXES {
            if canonical_target.starts_with(critical) {
                bail!(
                    "sandbox: cannot access critical system path '{}'",
                    canonical_target.display()
                );
            }
        }

        if self.workspace != "/" && !canonical_target.starts_with(&canonical_workspace) {
            bail!(
                "sandbox: access outside workspace '{}' is not allowed",
                self.workspace
            );
        }

        Ok(())
    }

    fn absolutize_path(path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    }

    fn is_workspace_prefixed(path: &Path, workspace: &Path) -> bool {
        let normalized_path = Self::normalize_relative(path);
        let normalized_workspace = Self::normalize_relative(workspace);
        !normalized_workspace.as_os_str().is_empty()
            && normalized_path.starts_with(&normalized_workspace)
    }

    fn normalize_relative(path: &Path) -> PathBuf {
        path.components()
            .filter(|component| !matches!(component, Component::CurDir))
            .fold(PathBuf::new(), |mut acc, component| {
                acc.push(component.as_os_str());
                acc
            })
    }
}

#[async_trait]
impl Tool for FilesystemTool {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn description(&self) -> &str {
        "Read, write, append, delete, or list files on the local filesystem."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "write", "append", "delete", "list"],
                    "description": "The filesystem action to perform"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory path"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write or append (required for write/append)"
                }
            },
            "required": ["action", "path"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let outcome: Result<Value> = async {
            let action = Self::require_action(&input)?;
            let raw_path = Self::require_path(&input)?;
            let resolved_path = self.resolve_path(&raw_path);
            self.enforce_path_safety(&resolved_path)?;
            let path_str = resolved_path.to_string_lossy().to_string();

            match action.as_str() {
                "read" => {
                    info!("filesystem read: {}", path_str);
                    let content = tokio::fs::read_to_string(&resolved_path)
                        .await
                        .with_context(|| format!("failed to read file '{}'", path_str))?;

                    Ok(Self::success_response(
                        "read",
                        &path_str,
                        json!({ "content": content }),
                    ))
                }
                "write" => {
                    let content = Self::require_content(&input, "write")?;
                    info!("filesystem write: {}", path_str);

                    if let Some(parent) = resolved_path.parent() {
                        tokio::fs::create_dir_all(parent).await.with_context(|| {
                            format!("failed to create parent directory for '{}'", path_str)
                        })?;
                    }

                    tokio::fs::write(&resolved_path, content.as_bytes())
                        .await
                        .with_context(|| format!("failed to write file '{}'", path_str))?;

                    Ok(Self::success_response(
                        "write",
                        &path_str,
                        json!({ "bytes_written": content.len() }),
                    ))
                }
                "append" => {
                    let content = Self::require_content(&input, "append")?;
                    info!("filesystem append: {}", path_str);

                    if let Some(parent) = resolved_path.parent() {
                        tokio::fs::create_dir_all(parent).await.with_context(|| {
                            format!("failed to create parent directory for '{}'", path_str)
                        })?;
                    }

                    let mut file = tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&resolved_path)
                        .await
                        .with_context(|| {
                            format!("failed to open file '{}' for append", path_str)
                        })?;
                    file.write_all(content.as_bytes())
                        .await
                        .with_context(|| format!("failed to append to file '{}'", path_str))?;

                    Ok(Self::success_response(
                        "append",
                        &path_str,
                        json!({ "bytes_written": content.len() }),
                    ))
                }
                "delete" => {
                    info!("filesystem delete: {}", path_str);
                    tokio::fs::remove_file(&resolved_path)
                        .await
                        .with_context(|| format!("failed to delete file '{}'", path_str))?;

                    Ok(Self::success_response("delete", &path_str, json!({})))
                }
                "list" => {
                    info!("filesystem list: {}", path_str);
                    let mut dir = tokio::fs::read_dir(&resolved_path)
                        .await
                        .with_context(|| format!("failed to list directory '{}'", path_str))?;

                    let mut files = Vec::new();
                    while let Some(entry) = dir.next_entry().await.with_context(|| {
                        format!("failed while iterating directory '{}'", path_str)
                    })? {
                        files.push(entry.file_name().to_string_lossy().to_string());
                    }
                    files.sort();

                    Ok(Self::success_response(
                        "list",
                        &path_str,
                        json!({ "files": files }),
                    ))
                }
                _ => Ok(Self::error_response(format!(
                    "unsupported filesystem action: {}",
                    action
                ))),
            }
        }
        .await;

        Ok(match outcome {
            Ok(value) => value,
            Err(error) => {
                warn!("filesystem operation failed: {}", error);
                Self::error_response(error.to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FilesystemTool;
    use std::path::PathBuf;

    #[test]
    fn resolve_path_does_not_double_prefix_workspace_relative_paths() {
        let tool = FilesystemTool::new("./workspace".to_string());

        let resolved = tool.resolve_path("./workspace/rust.txt");
        assert_eq!(resolved, PathBuf::from("./workspace/rust.txt"));
    }

    #[test]
    fn resolve_path_joins_plain_relative_paths_under_workspace() {
        let tool = FilesystemTool::new("./workspace".to_string());

        let resolved = tool.resolve_path("rust.txt");
        assert_eq!(resolved, PathBuf::from("./workspace/rust.txt"));
    }
}
