use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use enigo::{Axis, Enigo, Mouse, Settings};
use serde_json::Value;

pub struct ScrollTool {}

impl ScrollTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for ScrollTool {
    fn name(&self) -> &str {
        "scroll"
    }

    fn description(&self) -> &str {
        "Simulates a mouse scroll wheel operation on the current active window or coordinate. Actions: 'scroll_up' or 'scroll_down' (requires 'amount' in lines/pixels)."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The direction to scroll: 'scroll_up' or 'scroll_down'",
                    "enum": ["scroll_up", "scroll_down"]
                },
                "amount": {
                    "type": "integer",
                    "description": "The amount (roughly in virtual lines or ticks depending on the OS) to scroll."
                }
            },
            "required": ["action", "amount"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .context("Missing action")?;
        let amount = input
            .get("amount")
            .and_then(|v| v.as_i64())
            .context("Missing amount")? as i32;

        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("Enigo error: {:?}", e))?;

        match action {
            "scroll_up" => {
                enigo
                    .scroll(amount, Axis::Vertical)
                    .map_err(|e| anyhow::anyhow!("Scroll error: {:?}", e))?;
                Ok(serde_json::json!({ "status": format!("scrolled up by {}", amount) }))
            }
            "scroll_down" => {
                enigo
                    .scroll(-amount, Axis::Vertical)
                    .map_err(|e| anyhow::anyhow!("Scroll error: {:?}", e))?;
                Ok(serde_json::json!({ "status": format!("scrolled down by {}", amount) }))
            }
            _ => anyhow::bail!("Unknown scroll action: {}", action),
        }
    }
}
