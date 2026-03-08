use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use enigo::{Enigo, Mouse, Settings};
use serde_json::Value;

pub struct CursorPositionTool {}

impl CursorPositionTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for CursorPositionTool {
    fn name(&self) -> &str {
        "get_mouse_position"
    }

    fn description(&self) -> &str {
        "Returns the absolute current (X, Y) pixel coordinates of the hardware mouse cursor on the active display."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _input: Value) -> Result<Value> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("Enigo error: {:?}", e))?;
        let (x, y) = enigo
            .location()
            .map_err(|e| anyhow::anyhow!("Failed to read cursor pos: {:?}", e))?;

        Ok(serde_json::json!({
            "x": x,
            "y": y
        }))
    }
}
