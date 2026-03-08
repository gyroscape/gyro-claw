use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use crate::agent::indexer::SemanticIndexer;
use crate::tools::Tool;

pub struct SemanticSearchTool {
    indexer: SemanticIndexer,
}

impl SemanticSearchTool {
    pub fn new(indexer: SemanticIndexer) -> Self {
        Self { indexer }
    }
}

#[async_trait]
impl Tool for SemanticSearchTool {
    fn name(&self) -> &str {
        "semantic_search"
    }

    fn description(&self) -> &str {
        "Search the codebase by conceptual meaning instead of exact keywords. Use natural language queries like 'where is authentication configured' or 'how does the database connection work'. Note: the codebase must be indexed first via the CLI for this to return results."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The natural language question or concept to search the codebase for"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of code chunks to return (default is 5, max 15)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(15) as usize;

        if query.is_empty() {
            anyhow::bail!("Missing or empty 'query' parameter");
        }

        let results = self.indexer.search(&query, limit).await?;

        if results.is_empty() {
            return Ok(json!({
                "message": "No semantic matches found. Either the concept isn't present, or the codebase hasn't been indexed. Ask the user to run `gyro-claw index` if needed."
            }));
        }

        let formatted: Vec<_> = results
            .into_iter()
            .map(|(path, text, score)| {
                json!({
                    "file": path,
                    "relevance_score": score,
                    "code_snippet": text
                })
            })
            .collect();

        Ok(json!({
            "query": query,
            "results": formatted
        }))
    }
}
