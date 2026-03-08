use crate::agent::planner::Message;
use crate::llm::client::LlmClient;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct UiDetectorTool {
    workspace: PathBuf,
    llm: LlmClient,
}

impl UiDetectorTool {
    pub fn new(workspace_dir: &str, llm: LlmClient) -> Self {
        Self {
            workspace: PathBuf::from(workspace_dir),
            llm,
        }
    }

    fn resolve_image_path(&self, input: &Value) -> anyhow::Result<PathBuf> {
        if let Some(image_path) = input.get("image_path").and_then(|v| v.as_str()) {
            let mut resolved = if image_path.starts_with("./workspace/") {
                self.workspace
                    .join(image_path.trim_start_matches("./workspace/"))
            } else if image_path.starts_with("workspace/") {
                self.workspace
                    .join(image_path.trim_start_matches("workspace/"))
            } else if Path::new(image_path).is_absolute() {
                PathBuf::from(image_path)
            } else {
                self.workspace.join(image_path.trim_start_matches("./"))
            };

            if !resolved.exists() {
                let fallback = self.workspace.join("screenshots").join(
                    image_path
                        .trim_start_matches("./workspace/screenshots/")
                        .trim_start_matches("workspace/screenshots/")
                        .trim_start_matches("./screenshots/")
                        .trim_start_matches("screenshots/")
                        .trim_start_matches("./"),
                );
                if fallback.exists() {
                    resolved = fallback;
                }
            }

            if !resolved.exists() {
                return Err(anyhow::anyhow!("screenshot file not found: {}", image_path));
            }

            let workspace_canon = self
                .workspace
                .canonicalize()
                .unwrap_or(self.workspace.clone());
            let resolved_canon = match resolved.canonicalize() {
                Ok(path) => path,
                Err(err) => {
                    return Err(anyhow::anyhow!("failed to resolve image path: {}", err));
                }
            };
            if !resolved_canon.starts_with(&workspace_canon) {
                return Err(anyhow::anyhow!(
                    "sandbox violation: access to files outside workspace is blocked"
                ));
            }
            return Ok(resolved_canon);
        }

        if let Some(image_base64) = input.get("image_base64").and_then(|v| v.as_str()) {
            let bytes = match general_purpose::STANDARD.decode(image_base64) {
                Ok(bytes) => bytes,
                Err(err) => {
                    return Err(anyhow::anyhow!("failed to decode image_base64: {}", err));
                }
            };

            let screenshots_dir = self.workspace.join("screenshots");
            if let Err(err) = std::fs::create_dir_all(&screenshots_dir) {
                return Err(anyhow::anyhow!(
                    "failed to prepare screenshots directory: {}",
                    err
                ));
            }

            let filename = format!(
                "ui_detector_input_{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            );
            let path = screenshots_dir.join(filename);
            if let Err(err) = std::fs::write(&path, bytes) {
                return Err(anyhow::anyhow!("failed to save input image: {}", err));
            }
            return Ok(path);
        }

        Err(anyhow::anyhow!("missing image_path or image_base64"))
    }
}

#[async_trait]
impl Tool for UiDetectorTool {
    fn name(&self) -> &str {
        "detect_ui_elements"
    }

    fn description(&self) -> &str {
        "Analyzes a screenshot to find UI elements and returns bounding boxes. Provide an image path and a hint describing the target element."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "image_path": {
                    "type": "string",
                    "description": "Path to screenshot inside workspace"
                },
                "image_base64": {
                    "type": "string",
                    "description": "Optional base64 encoded screenshot payload"
                },
                "hint": {
                    "type": "string",
                    "description": "Description of target element to detect"
                }
            },
            "oneOf": [
                { "required": ["image_path"] },
                { "required": ["image_base64"] }
            ]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let hint = input
            .get("hint")
            .and_then(|v| v.as_str())
            .unwrap_or("search bar");

        let resolved_path = self.resolve_image_path(&input)?;

        // OpenRouter-compatible: content must be a single string, not multimodal arrays.
        let user_prompt = format!(
            "Analyze this screenshot and locate the UI element described as: {}.\n\
Screenshot file path: {}.\n\
Return ONLY JSON.\n\
If found, return:\n\
{{\"element\":\"<name>\",\"x\":420,\"y\":220,\"width\":540,\"height\":40,\"confidence\":0.9}}\n\
If not found, return:\n\
{{\"status\":\"error\",\"error_type\":\"ui_not_found\"}}",
            hint,
            resolved_path.display()
        );

        let system_prompt =
            "You are a UI detection assistant. Respond with strict JSON only and no markdown.";

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: Value::String(system_prompt.to_string()),
            },
            Message {
                role: "user".to_string(),
                content: Value::String(user_prompt),
            },
        ];

        let raw_response = match self.llm.chat(&messages).await {
            Ok(text) => text,
            Err(err) => {
                return Ok(error_response(
                    "ui_detector",
                    "ui_detection_failed",
                    format!("LLM request failed: {}", err),
                    "Retry with a fresh screenshot.",
                ));
            }
        };

        let parsed = match parse_json_payload(&raw_response) {
            Some(v) => v,
            None => {
                return Ok(error_response(
                    "ui_detector",
                    "ui_detection_parse_failed",
                    "model output was not valid JSON",
                    "Take a new screenshot and retry.",
                ));
            }
        };

        if parsed
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("error"))
            .unwrap_or(false)
            || parsed.get("error_type").and_then(|v| v.as_str()) == Some("ui_not_found")
        {
            return Ok(json!({
                "status": "error",
                "tool": "ui_detector",
                "error_type": "ui_not_found",
                "message": "requested UI element not found",
                "hint": format!("{} not visible", hint),
                "suggestion": "scroll or capture new screenshot"
            }));
        }

        let bbox = match extract_bbox(&parsed, hint) {
            Some(v) => v,
            None => {
                return Ok(json!({
                    "status": "error",
                    "tool": "ui_detector",
                    "error_type": "ui_not_found",
                    "message": "requested UI element not found",
                    "hint": format!("{} not visible", hint),
                    "suggestion": "scroll or capture new screenshot"
                }));
            }
        };

        Ok(json!({
            "status": "success",
            "result": bbox,
            "elements": [bbox]
        }))
    }
}

fn extract_bbox(value: &Value, hint: &str) -> Option<Value> {
    match value {
        Value::Array(arr) => arr.iter().find_map(|item| extract_bbox(item, hint)),
        Value::Object(map) => {
            if let Some(v) = map.get("result").and_then(|v| extract_bbox(v, hint)) {
                return Some(v);
            }
            if let Some(v) = map.get("elements").and_then(|v| extract_bbox(v, hint)) {
                return Some(v);
            }
            if let Some(v) = map.get("bounding_box").and_then(|v| extract_bbox(v, hint)) {
                return Some(v);
            }

            let x = map
                .get("x")
                .and_then(as_i64)
                .or_else(|| map.get("left").and_then(as_i64));
            let y = map
                .get("y")
                .and_then(as_i64)
                .or_else(|| map.get("top").and_then(as_i64));

            if let (Some(x), Some(y)) = (x, y) {
                let width = map
                    .get("width")
                    .and_then(as_i64)
                    .or_else(|| {
                        let right = map.get("right").and_then(as_i64)?;
                        let left = map.get("left").and_then(as_i64)?;
                        Some((right - left).max(1))
                    })
                    .unwrap_or(1);
                let height = map
                    .get("height")
                    .and_then(as_i64)
                    .or_else(|| {
                        let bottom = map.get("bottom").and_then(as_i64)?;
                        let top = map.get("top").and_then(as_i64)?;
                        Some((bottom - top).max(1))
                    })
                    .unwrap_or(1);
                let element = map
                    .get("element")
                    .or_else(|| map.get("label"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(hint)
                    .to_string();
                let confidence = map.get("confidence").and_then(as_f64).unwrap_or(0.5);
                return Some(json!({
                    "element": element,
                    "x": x,
                    "y": y,
                    "width": width.max(1),
                    "height": height.max(1),
                    "center_x": x + width.max(1) / 2,
                    "center_y": y + height.max(1) / 2,
                    "confidence": confidence
                }));
            }
            None
        }
        _ => None,
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    value.as_str()?.trim().parse::<i64>().ok()
}

fn as_f64(value: &Value) -> Option<f64> {
    if let Some(n) = value.as_f64() {
        return Some(n);
    }
    value.as_str()?.trim().parse::<f64>().ok()
}

fn parse_json_payload(raw: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Some(value);
    }

    let cleaned = raw
        .replace("```json", "")
        .replace("```", "")
        .trim()
        .to_string();
    if let Ok(value) = serde_json::from_str::<Value>(&cleaned) {
        return Some(value);
    }

    let start = cleaned.find('{')?;
    let end = cleaned.rfind('}')?;
    serde_json::from_str::<Value>(&cleaned[start..=end]).ok()
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
