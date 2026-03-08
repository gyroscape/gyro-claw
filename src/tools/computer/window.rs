use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Command;

use crate::tools::Tool;

pub const PREFERRED_BROWSERS: &[&str] = &["Google Chrome", "Safari", "Chromium", "Arc", "Firefox"];

pub fn detect_active_browser_window() -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        let active_app = active_window_name_macos()?;
        if PREFERRED_BROWSERS
            .iter()
            .any(|candidate| *candidate == active_app)
        {
            return Ok(Some(active_app));
        }
        Ok(None)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

pub fn focus_browser_window() -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        if let Some(active_browser) = detect_active_browser_window()? {
            focus_app_macos(&active_browser)?;
            return Ok(Some(active_browser));
        }

        for browser in PREFERRED_BROWSERS {
            if is_app_running_macos(browser)? {
                focus_app_macos(browser)?;
                return Ok(Some((*browser).to_string()));
            }
        }

        Ok(None)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn run_script(script: &str, context_msg: &str) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .with_context(|| context_msg.to_string())?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("AppleScript failed: {}", err.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "macos")]
fn active_window_name_macos() -> Result<String> {
    run_script(
        r#"
            tell application "System Events"
                return name of first application process whose frontmost is true
            end tell
        "#,
        "Failed to query active window",
    )
}

#[cfg(target_os = "macos")]
fn is_app_running_macos(app_name: &str) -> Result<bool> {
    let script = format!(
        r#"
            tell application "System Events"
                return exists application process "{}"
            end tell
        "#,
        app_name
    );
    let result = run_script(&script, "Failed to query running app state")?;
    Ok(result.eq_ignore_ascii_case("true"))
}

#[cfg(target_os = "macos")]
fn focus_app_macos(app_name: &str) -> Result<()> {
    let script = format!(
        r#"
            tell application "{}"
                activate
            end tell
        "#,
        app_name
    );
    run_script(&script, "Failed to focus target app")?;
    Ok(())
}

pub struct WindowTool {}

impl WindowTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for WindowTool {
    fn name(&self) -> &str {
        "window"
    }

    fn description(&self) -> &str {
        "Manage desktop application windows. Use this to find active applications, detect browser focus, or bring a specific app to the foreground. Actions: 'list_windows', 'focus_window' (requires 'app_name'), 'focus_browser', 'detect_active_browser', 'get_active_window'."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action to perform: 'list_windows', 'focus_window', 'focus_browser', 'detect_active_browser', 'get_active_window'",
                    "enum": ["list_windows", "focus_window", "focus_browser", "detect_active_browser", "get_active_window"]
                },
                "app_name": {
                    "type": "string",
                    "description": "The exact name of the application to focus. Required for 'focus_window'. E.g. 'Google Chrome' or 'Safari'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let Some(action) = input.get("action").and_then(|v| v.as_str()) else {
            return Ok(error_response(
                "window",
                "invalid_input",
                "missing action",
                "Provide an action from the schema enum.",
            ));
        };

        match action {
            "list_windows" => {
                #[cfg(not(target_os = "macos"))]
                {
                    return Ok(error_response(
                        "window",
                        "unsupported_platform",
                        "list_windows currently supports only macOS",
                        "Use platform-specific window integration for this OS.",
                    ));
                }

                #[cfg(target_os = "macos")]
                {
                    // Get all application names with visible windows
                    let script = r#"
                    tell application "System Events"
                        set visibleApps to name of (application processes whose visible is true)
                    end tell
                    return visibleApps
                "#;

                    let output = Command::new("osascript")
                        .arg("-e")
                        .arg(script)
                        .output()
                        .context("Failed to execute osascript")?;

                    if !output.status.success() {
                        let err = String::from_utf8_lossy(&output.stderr);
                        return Ok(error_response(
                            "window",
                            "window_query_failed",
                            format!("failed to list windows: {}", err.trim()),
                            "Grant accessibility permission to the terminal process.",
                        ));
                    }

                    let apps_str = String::from_utf8_lossy(&output.stdout);
                    // AppleScript returns comma-separated list
                    let apps: Vec<String> = apps_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();

                    Ok(json!({ "windows": apps }))
                }
            }
            "focus_window" => {
                let Some(app_name) = input.get("app_name").and_then(|v| v.as_str()) else {
                    return Ok(error_response(
                        "window",
                        "invalid_input",
                        "missing app_name for focus_window",
                        "Provide the target app_name (e.g. Google Chrome).",
                    ));
                };

                #[cfg(target_os = "macos")]
                {
                    if let Err(err) = focus_app_macos(app_name) {
                        return Ok(error_response(
                            "window",
                            "focus_failed",
                            format!("failed to focus '{}': {}", app_name, err),
                            "Make sure the app is installed and visible.",
                        ));
                    }
                    return Ok(json!({ "status": "ok", "focused_app": app_name }));
                }

                #[cfg(not(target_os = "macos"))]
                {
                    Ok(error_response(
                        "window",
                        "unsupported_platform",
                        "focus_window currently supports only macOS",
                        "Use platform-specific focus integration for this OS.",
                    ))
                }
            }
            "focus_browser" => match focus_browser_window() {
                Ok(Some(browser)) => Ok(json!({ "status": "ok", "focused_app": browser })),
                Ok(None) => Ok(error_response(
                    "window",
                    "browser_not_found",
                    "no supported browser window is currently running",
                    "Launch Safari/Chrome and retry.",
                )),
                Err(err) => Ok(error_response(
                    "window",
                    "focus_failed",
                    format!("failed to focus browser: {}", err),
                    "Grant accessibility permission and retry.",
                )),
            },
            "detect_active_browser" => match detect_active_browser_window() {
                Ok(Some(browser)) => Ok(json!({ "status": "ok", "active_browser": browser })),
                Ok(None) => Ok(error_response(
                    "window",
                    "browser_not_active",
                    "active window is not a supported browser",
                    "Bring Chrome/Safari to front or use focus_browser.",
                )),
                Err(err) => Ok(error_response(
                    "window",
                    "window_query_failed",
                    format!("failed to detect active browser: {}", err),
                    "Grant accessibility permission and retry.",
                )),
            },
            "get_active_window" => {
                #[cfg(not(target_os = "macos"))]
                {
                    return Ok(error_response(
                        "window",
                        "unsupported_platform",
                        "get_active_window currently supports only macOS",
                        "Use platform-specific active-window integration.",
                    ));
                }

                #[cfg(target_os = "macos")]
                {
                    let script = r#"
                    tell application "System Events"
                        set frontApp to name of first application process whose frontmost is true
                    end tell
                    return frontApp
                "#;

                    let output = Command::new("osascript")
                        .arg("-e")
                        .arg(script)
                        .output()
                        .context("Failed to get active window via osascript")?;

                    if !output.status.success() {
                        let err = String::from_utf8_lossy(&output.stderr);
                        return Ok(error_response(
                            "window",
                            "window_query_failed",
                            format!("failed to get active window: {}", err.trim()),
                            "Grant accessibility permission and retry.",
                        ));
                    }

                    let active_app = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    Ok(json!({ "status": "ok", "active_app": active_app }))
                }
            }
            _ => Ok(error_response(
                "window",
                "invalid_action",
                format!("unknown window action: {}", action),
                "Use one of the documented window actions.",
            )),
        }
    }
}

fn error_response(
    tool: &str,
    error_type: &str,
    message: impl Into<String>,
    suggestion: &str,
) -> Value {
    json!({
        "status": "error",
        "tool": tool,
        "error_type": error_type,
        "message": message.into(),
        "suggestion": suggestion
    })
}
