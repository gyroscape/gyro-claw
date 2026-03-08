//! # Code Search Tool
//!
//! Search project files for keywords without using shell.
//! Supports filtering by file extension and returns matching lines.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use walkdir::WalkDir;

use super::Tool;

pub struct SearchTool;

impl SearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search project files for keywords. Returns matching file paths and lines. \
         Supports filtering by file extension."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The keyword or phrase to search for"
                },
                "directory": {
                    "type": "string",
                    "description": "Directory to search in (default: current directory)"
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "File extensions to filter (e.g. [\"rs\", \"js\", \"ts\"])"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 50)"
                }
            },
            "required": ["query"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let query = input["query"].as_str().context("Missing 'query' field")?;
        let directory = input["directory"].as_str().unwrap_or(".");
        let max_results = input["max_results"].as_u64().unwrap_or(50) as usize;

        let extensions: Vec<String> = input
            .get("extensions")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let mut results = Vec::new();
        let query_lower = query.to_lowercase();

        for entry in WalkDir::new(directory)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if results.len() >= max_results {
                break;
            }

            let path = entry.path();

            // Skip directories, hidden files, and target/node_modules
            if !path.is_file() {
                continue;
            }
            let path_str = path.to_string_lossy();
            if path_str.contains("/target/")
                || path_str.contains("/node_modules/")
                || path_str.contains("/.git/")
            {
                continue;
            }

            // Filter by extension if specified
            if !extensions.is_empty() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !extensions.iter().any(|e| e == ext) {
                    continue;
                }
            }

            // Read and search file
            if let Ok(content) = std::fs::read_to_string(path) {
                for (line_num, line) in content.lines().enumerate() {
                    if results.len() >= max_results {
                        break;
                    }
                    if line.to_lowercase().contains(&query_lower) {
                        results.push(serde_json::json!({
                            "file": path_str.to_string(),
                            "line_number": line_num + 1,
                            "content": line.trim(),
                        }));
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "success": true,
            "query": query,
            "total_matches": results.len(),
            "results": results,
        }))
    }
}
