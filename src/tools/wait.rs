use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tools::Tool;

pub struct WaitTool;

impl WaitTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &str {
        "wait_for"
    }

    fn description(&self) -> &str {
        "Wait for a short duration in seconds before continuing. Useful to allow pages or UI transitions to settle."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "timeout": {
                    "type": "integer",
                    "description": "Number of seconds to wait. Defaults to 3."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let seconds = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(3);
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        Ok(json!({
            "status": "success",
            "result": {
                "wait_complete": true,
                "timeout_seconds": seconds
            }
        }))
    }
}
