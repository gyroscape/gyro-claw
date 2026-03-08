use crate::tools::computer::window::focus_browser_window;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use screenshots::Screen;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

const MAX_CAPTURE_RETRIES: usize = 3;
const WHITE_PIXEL_THRESHOLD: f64 = 0.97;
const LUMINANCE_VARIANCE_THRESHOLD: f64 = 4.0;

pub struct ScreenshotTool {
    workspace: PathBuf,
}

impl ScreenshotTool {
    pub fn new(workspace_dir: &str) -> Self {
        Self {
            workspace: PathBuf::from(workspace_dir),
        }
    }
}

#[async_trait]
impl Tool for ScreenshotTool {
    fn name(&self) -> &str {
        "screenshot"
    }

    fn description(&self) -> &str {
        "Captures a screenshot of the main monitor. Use this to observe the current state of the UI before making mouse or keyboard interactions. Returns the raw base64 image and the path where it was saved in the workspace."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "display_id": {
                    "type": "integer",
                    "description": "Optional display index to capture. Defaults to the primary monitor (0)."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let display_id = input
            .get("display_id")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let screens = match Screen::all() {
            Ok(v) => v,
            Err(err) => {
                return Ok(error_response(
                    "screenshot",
                    "capture_failed",
                    format!("failed to enumerate displays: {}", err),
                    "Ensure screen capture permissions are granted.",
                ));
            }
        };

        let Some(screen) = screens.get(display_id) else {
            return Ok(error_response(
                "screenshot",
                "invalid_display",
                format!("display {} not found", display_id),
                "Use display_id from 0..number_of_displays-1.",
            ));
        };

        let display_info = screen.display_info;
        let mut focused_browser: Option<String> = None;
        let mut last_white_ratio = 0.0_f64;
        let mut last_variance = 0.0_f64;

        for attempt in 1..=MAX_CAPTURE_RETRIES {
            if focused_browser.is_none() {
                focused_browser = focus_browser_window().ok().flatten();
            } else {
                let _ = focus_browser_window();
            }
            tokio::time::sleep(Duration::from_millis(500)).await;

            let image = match screen.capture() {
                Ok(img) => img,
                Err(err) => {
                    return Ok(error_response(
                        "screenshot",
                        "capture_failed",
                        format!("failed to capture screenshot: {}", err),
                        "Check screen recording permissions and retry.",
                    ));
                }
            };

            let (white_ratio, variance) = compute_image_blankness(&image);
            last_white_ratio = white_ratio;
            last_variance = variance;

            if is_blank_capture(white_ratio, variance) {
                if attempt < MAX_CAPTURE_RETRIES {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                return Ok(serde_json::json!({
                    "status": "error",
                    "tool": "screenshot",
                    "error_type": "blank_screenshot",
                    "message": "captured screenshot appears blank after retries",
                    "suggestion": "Ensure the browser window is visible, then capture a new screenshot.",
                    "retries": MAX_CAPTURE_RETRIES,
                    "blankness": {
                        "white_ratio": white_ratio,
                        "variance": variance
                    }
                }));
            }

            let mut cursor = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut cursor, image::ImageFormat::Png)
                .map_err(|e| anyhow::anyhow!("Failed to encode image to PNG: {:?}", e))?;
            let buffer = cursor.into_inner();

            let b64 = general_purpose::STANDARD.encode(&buffer);

            let screenshots_dir = self.workspace.join("screenshots");
            std::fs::create_dir_all(&screenshots_dir)?;

            let filename = format!(
                "screen_{}_{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                rand::random::<u32>()
            );
            let filepath = screenshots_dir.join(&filename);
            image
                .save(&filepath)
                .context("Failed to save screenshot payload")?;

            let relative_path = format!("./workspace/screenshots/{}", filename);

            return Ok(serde_json::json!({
                "status": "ok",
                "display_id": display_id,
                "width": display_info.width,
                "height": display_info.height,
                "timestamp": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                "image_path": relative_path,
                "image_base64": b64,
                "base64": b64,
                "focused_browser": focused_browser,
                "blankness": {
                    "white_ratio": white_ratio,
                    "variance": variance
                }
            }));
        }

        Ok(serde_json::json!({
            "status": "error",
            "tool": "screenshot",
            "error_type": "blank_screenshot",
            "message": "unable to capture a non-blank screenshot",
            "suggestion": "Bring the browser to front and retry capture.",
            "blankness": {
                "white_ratio": last_white_ratio,
                "variance": last_variance
            }
        }))
    }
}

fn compute_image_blankness(image: &image::RgbaImage) -> (f64, f64) {
    let mut white_pixels = 0usize;
    let mut n = 0f64;
    let mut mean = 0f64;
    let mut m2 = 0f64;

    for pixel in image.pixels() {
        let r = pixel[0] as f64;
        let g = pixel[1] as f64;
        let b = pixel[2] as f64;

        if r >= 245.0 && g >= 245.0 && b >= 245.0 {
            white_pixels += 1;
        }

        let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        n += 1.0;
        let delta = luminance - mean;
        mean += delta / n;
        let delta2 = luminance - mean;
        m2 += delta * delta2;
    }

    if n <= 1.0 {
        return (1.0, 0.0);
    }

    let white_ratio = white_pixels as f64 / n;
    let variance = m2 / (n - 1.0);
    (white_ratio, variance)
}

fn is_blank_capture(white_ratio: f64, variance: f64) -> bool {
    white_ratio >= WHITE_PIXEL_THRESHOLD || variance <= LUMINANCE_VARIANCE_THRESHOLD
}

fn error_response(
    tool: &str,
    error_type: &str,
    message: impl Into<String>,
    suggestion: &str,
) -> Value {
    serde_json::json!({
        "status": "error",
        "tool": tool,
        "error_type": error_type,
        "message": message.into(),
        "suggestion": suggestion
    })
}
