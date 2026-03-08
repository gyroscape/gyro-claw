use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Command;

pub struct SystemTool {
    allowed_apps: Vec<String>,
}

impl SystemTool {
    pub fn new(allowed_apps: Vec<String>) -> Self {
        Self { allowed_apps }
    }
}

#[async_trait]
impl Tool for SystemTool {
    fn name(&self) -> &str {
        "system"
    }

    fn description(&self) -> &str {
        "Automates the system application layer. Use this to open configured applications or launch URLs in the default browser."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The action to perform: 'open_app', 'open_url'",
                    "enum": ["open_app", "open_url"]
                },
                "name": {
                    "type": "string",
                    "description": "App name to open. Required for open_app."
                },
                "url": {
                    "type": "string",
                    "description": "URL to open. Required for open_url."
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

        match action {
            "open_app" => {
                let name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .context("Missing app name")?;
                if !self.allowed_apps.contains(&name.to_string()) {
                    anyhow::bail!(
                        "Application '{}' is not in the allowed_apps config list.",
                        name
                    );
                }

                #[cfg(target_os = "macos")]
                Command::new("open").arg("-a").arg(name).spawn()?;

                #[cfg(target_os = "linux")]
                Command::new(name).spawn()?;

                #[cfg(target_os = "windows")]
                Command::new("cmd")
                    .arg("/C")
                    .arg("start")
                    .arg(name)
                    .spawn()?;

                Ok(serde_json::json!({ "status": format!("opened app {}", name) }))
            }
            "open_url" => {
                let url = input
                    .get("url")
                    .and_then(|v| v.as_str())
                    .context("Missing url")?;

                #[cfg(target_os = "macos")]
                Command::new("open").arg(url).spawn()?;

                #[cfg(target_os = "linux")]
                Command::new("xdg-open").arg(url).spawn()?;

                #[cfg(target_os = "windows")]
                Command::new("cmd")
                    .arg("/C")
                    .arg("start")
                    .arg(url)
                    .spawn()?;

                Ok(serde_json::json!({ "status": format!("opened url {}", url) }))
            }
            _ => anyhow::bail!("Unknown system action: {}", action),
        }
    }
}
