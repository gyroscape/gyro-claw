use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde_json::Value;

pub struct KeyboardTool {}

impl KeyboardTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for KeyboardTool {
    fn name(&self) -> &str {
        "keyboard"
    }

    fn description(&self) -> &str {
        "Control the computer keyboard. Actions: 'type_text' (requires 'text'), 'press_key' (requires 'key' e.g. 'enter', 'tab', 'escape', 'space', 'backspace', 'up', 'down', 'left', 'right')."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The action to perform: 'type_text', 'press_key'",
                    "enum": ["type_text", "press_key"]
                },
                "text": {
                    "type": "string",
                    "description": "Text to type. Required if action is 'type_text'."
                },
                "key": {
                    "type": "string",
                    "description": "Key to press. Required if action is 'press_key'. Examples: 'enter', 'tab', 'escape'."
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

        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("Enigo error: {:?}", e))?;

        match action {
            "type_text" => {
                let text = input
                    .get("text")
                    .and_then(|v| v.as_str())
                    .context("Missing text")?;
                println!("Executing action: keyboard type '{}'", text);
                enigo
                    .text(text)
                    .map_err(|e| anyhow::anyhow!("Keyboard error: {:?}", e))?;
                Ok(serde_json::json!({ "status": format!("typed '{}'", text) }))
            }
            "press_key" => {
                let key_str = input
                    .get("key")
                    .and_then(|v| v.as_str())
                    .context("Missing key")?;
                println!("Executing action: keyboard press '{}'", key_str);
                let key = match key_str.to_lowercase().as_str() {
                    "enter" | "return" => Key::Return,
                    "tab" => Key::Tab,
                    "space" => Key::Space,
                    "backspace" => Key::Backspace,
                    "escape" | "esc" => Key::Escape,
                    "up" => Key::UpArrow,
                    "down" => Key::DownArrow,
                    "left" => Key::LeftArrow,
                    "right" => Key::RightArrow,
                    c if c.len() == 1 => Key::Unicode(c.chars().next().unwrap()),
                    _ => anyhow::bail!("Unsupported key: {}", key_str),
                };
                enigo
                    .key(key, Direction::Click)
                    .map_err(|e| anyhow::anyhow!("Keyboard error: {:?}", e))?;
                Ok(serde_json::json!({ "status": format!("pressed '{}'", key_str) }))
            }
            _ => anyhow::bail!("Unknown keyboard action: {}", action),
        }
    }
}
