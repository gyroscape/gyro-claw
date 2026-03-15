//! # Tool System
//!
//! Defines the `Tool` trait and `ToolRegistry` for the modular tool architecture.
//! Tools expose name, description, input schema, and an async execute function.
//! The AI model only sees tool descriptions and schemas — never secrets or internals.

pub mod browser;
pub mod computer;
pub mod edit;
pub mod filesystem;
pub mod git;
pub mod http;
pub mod playwright;
pub mod project_map;
pub mod search;
pub mod semantic_search;
pub mod shell;
pub mod test_runner;
pub mod wait;
pub mod web_fetch;
pub mod web_search;
pub mod sub_agents;
pub mod skills;
pub mod skills_tool;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

/// Secret policy declared by each tool.
#[derive(Debug, Clone, Default)]
pub struct ToolSecretPolicy {
    pub allow_secrets: bool,
    pub allowed_secret_keys: Vec<String>,
}

impl ToolSecretPolicy {
    pub fn deny() -> Self {
        Self::default()
    }

    pub fn allow(keys: &[&str]) -> Self {
        Self {
            allow_secrets: true,
            allowed_secret_keys: keys.iter().map(|k| (*k).to_string()).collect(),
        }
    }
}

/// Every tool must implement this trait.
/// The AI model receives `name()`, `description()`, and `input_schema()` only.
/// `execute()` is called by the secure executor, never by the AI directly.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g. "shell", "filesystem", "http")
    fn name(&self) -> &str;

    /// Human-readable description shown to the AI model
    fn description(&self) -> &str;

    /// JSON Schema describing the expected input parameters
    fn input_schema(&self) -> Value;

    /// Whether the tool is inherently safe to run in parallel with other read-only tools.
    /// Tools with mixed read/write behavior should return false and rely on call-level checks.
    fn is_parallel_safe(&self) -> bool {
        false
    }

    /// Secret policy declaration for this tool.
    /// The executor intersects this with global config policy before resolving placeholders.
    fn secret_policy(&self) -> ToolSecretPolicy {
        ToolSecretPolicy::deny()
    }

    /// Execute the tool with the given input.
    /// This is called by the executor — not the AI model directly.
    async fn execute(&self, input: Value) -> Result<Value>;
}

/// Registry that holds all available tools, keyed by name.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. Overwrites any existing tool with the same name.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Return descriptions of all tools for the AI prompt.
    /// This intentionally excludes secrets and internal details.
    pub fn tool_descriptions(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "input_schema": tool.input_schema(),
                    "parallel_safe": tool.is_parallel_safe(),
                })
            })
            .collect()
    }

    /// List all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Suggest tool names close to a requested name.
    pub fn suggest_tools(&self, name: &str, limit: usize) -> Vec<String> {
        let trimmed = name.trim();
        if trimmed.is_empty() || limit == 0 {
            return Vec::new();
        }

        let mut scored: Vec<(usize, String)> = self
            .tools
            .keys()
            .map(|tool_name| (levenshtein(trimmed, tool_name), tool_name.clone()))
            .collect();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, name)| name).collect()
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }

    let b_len = b.chars().count();
    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row = vec![0; b_len + 1];

    for (i, ca) in a.chars().enumerate() {
        curr_row[0] = i + 1;
        let mut prev_diag = i;
        for (j, cb) in b.chars().enumerate() {
            let insert_cost = curr_row[j] + 1;
            let delete_cost = prev_row[j + 1] + 1;
            let replace_cost = if ca == cb { prev_diag } else { prev_diag + 1 };
            prev_diag = prev_row[j + 1];
            curr_row[j + 1] = insert_cost.min(delete_cost).min(replace_cost);
        }
        prev_row.clone_from_slice(&curr_row);
    }

    prev_row[b_len]
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
