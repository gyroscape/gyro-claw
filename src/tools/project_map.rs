//! # Project Map Tool
//!
//! Returns a tree view of the project structure.
//! Limits depth to avoid very large output.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use walkdir::WalkDir;

use super::Tool;

pub struct ProjectMapTool;

impl ProjectMapTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ProjectMapTool {
    fn name(&self) -> &str {
        "project_map"
    }

    fn description(&self) -> &str {
        "Return a tree view of the project directory structure. \
         Useful for understanding project layout."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "directory": {
                    "type": "string",
                    "description": "Root directory to map (default: current directory)"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth to traverse (default: 4)"
                },
                "show_files": {
                    "type": "boolean",
                    "description": "Whether to show files or only directories (default: true)"
                }
            }
        })
    }

    fn is_parallel_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let directory = input["directory"].as_str().unwrap_or(".");
        let max_depth = input["max_depth"].as_u64().unwrap_or(4) as usize;
        let show_files = input["show_files"].as_bool().unwrap_or(true);

        let root = Path::new(directory);
        if !root.exists() {
            return Ok(serde_json::json!({
                "success": false,
                "error": format!("Directory not found: {}", directory),
            }));
        }

        let mut tree = String::new();
        let mut entries: Vec<(usize, String, bool)> = Vec::new();

        for entry in WalkDir::new(directory)
            .max_depth(max_depth)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            let path_str = path.to_string_lossy();

            // Skip hidden directories and common build artifacts
            if path_str.contains("/.git/")
                || path_str.contains("/target/")
                || path_str.contains("/node_modules/")
                || path_str.contains("/.git")
                    && path != root
                    && entry.depth() > 0
                    && entry.file_name().to_string_lossy().starts_with('.')
            {
                continue;
            }

            let is_dir = path.is_dir();
            if !show_files && !is_dir {
                continue;
            }

            let depth = entry.depth();
            let name = entry.file_name().to_string_lossy().to_string();
            entries.push((depth, name, is_dir));
        }

        // Build tree string
        for (i, (depth, name, is_dir)) in entries.iter().enumerate() {
            if *depth == 0 {
                tree.push_str(&format!("{}/\n", name));
                continue;
            }

            let indent = "│   ".repeat(depth.saturating_sub(1));

            // Check if this is the last entry at this depth
            let is_last = entries
                .get(i + 1)
                .map(|(next_depth, _, _)| *next_depth <= *depth)
                .unwrap_or(true);

            let prefix = if is_last { "└── " } else { "├── " };
            let suffix = if *is_dir { "/" } else { "" };

            tree.push_str(&format!("{}{}{}{}\n", indent, prefix, name, suffix));
        }

        Ok(serde_json::json!({
            "success": true,
            "tree": tree,
            "total_entries": entries.len(),
        }))
    }
}
