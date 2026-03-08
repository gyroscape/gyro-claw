use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
use serde_json::Value;

pub struct MouseTool {}

impl MouseTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for MouseTool {
    fn name(&self) -> &str {
        "mouse"
    }

    fn description(&self) -> &str {
        "Control the computer mouse. Actions: 'move_mouse' (requires x, y), 'click' (optional x, y), 'double_click' (optional x, y), 'drag' (requires x, y), 'click_element' (requires x, y, and optionally 'label')."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The action to perform: 'move_mouse', 'click', 'double_click', 'drag', 'click_element'",
                    "enum": ["move_mouse", "click", "double_click", "drag", "click_element"]
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate on the screen. Required for move_mouse and drag. Optional for click and double_click."
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate on the screen. Required for move_mouse and drag. Optional for click and double_click."
                },
                "label": {
                    "type": "string",
                    "description": "Optional semantic name of the UI element being targeted if action is 'click_element'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .context("Missing action")?;
        let x = input.get("x").and_then(|v| v.as_i64()).map(|v| v as i32);
        let y = input.get("y").and_then(|v| v.as_i64()).map(|v| v as i32);

        println!("Executing action: mouse {} at ({:?}, {:?})", action, x, y);

        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("Enigo error: {:?}", e))?;

        match action {
            "move_mouse" => {
                let x = x.context("Missing x for move_mouse")?;
                let y = y.context("Missing y for move_mouse")?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                Ok(serde_json::json!({ "status": format!("moved to {},{}", x, y) }))
            }
            "click" => {
                if let (Some(x), Some(y)) = (x, y) {
                    enigo
                        .move_mouse(x, y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                }
                enigo
                    .button(Button::Left, Direction::Click)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                Ok(serde_json::json!({ "status": "clicked" }))
            }
            "click_element" => {
                let x = x.context("Missing exact x for click_element")?;
                let y = y.context("Missing exact y for click_element")?;
                let label = input
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown element");

                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                enigo
                    .button(Button::Left, Direction::Click)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                Ok(
                    serde_json::json!({ "status": format!("clicked element '{}' at {},{}", label, x, y) }),
                )
            }
            "double_click" => {
                if let (Some(x), Some(y)) = (x, y) {
                    enigo
                        .move_mouse(x, y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                }
                enigo
                    .button(Button::Left, Direction::Click)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                enigo
                    .button(Button::Left, Direction::Click)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                Ok(serde_json::json!({ "status": "double_clicked" }))
            }
            "drag" => {
                let x = x.context("Missing x for drag")?;
                let y = y.context("Missing y for drag")?;
                enigo
                    .button(Button::Left, Direction::Press)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                enigo
                    .button(Button::Left, Direction::Release)
                    .map_err(|e| anyhow::anyhow!("Mouse error: {:?}", e))?;
                Ok(serde_json::json!({ "status": format!("dragged to {},{}", x, y) }))
            }
            _ => anyhow::bail!("Unknown mouse action: {}", action),
        }
    }
}
