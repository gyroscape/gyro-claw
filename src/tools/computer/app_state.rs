use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Command;

pub struct AppStateTool {}

impl AppStateTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for AppStateTool {
    fn name(&self) -> &str {
        "app_state"
    }

    fn description(&self) -> &str {
        "Detect the active desktop application, the foreground window title, and the screen bounding box coordinates of that window limit. This is necessary to normalize clicks from absolute desktop space into relative window space coordinates."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _input: Value) -> Result<Value> {
        let script = r#"
            tell application "System Events"
                set frontApp to name of first application process whose frontmost is true
                set appProcess to first application process whose frontmost is true
                
                set winTitle to ""
                set winX to 0
                set winY to 0
                set winW to 0
                set winH to 0
                
                try
                    set frontWindow to first window of appProcess
                    set winTitle to name of frontWindow
                    set winPos to position of frontWindow
                    set winSize to size of frontWindow
                    
                    set winX to item 1 of winPos
                    set winY to item 2 of winPos
                    set winW to item 1 of winSize
                    set winH to item 2 of winSize
                end try
                
                return frontApp & "|||" & winTitle & "|||" & winX & "|||" & winY & "|||" & winW & "|||" & winH
            end tell
        "#;

        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .context("Failed to execute AppleScript for app_state")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("AppleScript failed: {}", err);
        }

        let result_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let parts: Vec<&str> = result_str.split("|||").collect();

        if parts.len() < 6 {
            anyhow::bail!("Unexpected AppleScript output format: {}", result_str);
        }

        let app_name = parts[0].to_string();
        let title = parts[1].to_string();

        let x: i32 = parts[2].parse().unwrap_or(0);
        let y: i32 = parts[3].parse().unwrap_or(0);
        let w: i32 = parts[4].parse().unwrap_or(0);
        let h: i32 = parts[5].parse().unwrap_or(0);

        Ok(serde_json::json!({
            "active_app": app_name,
            "title": title,
            "window_bounds": {
                "x": x,
                "y": y,
                "width": w,
                "height": h
            }
        }))
    }
}
