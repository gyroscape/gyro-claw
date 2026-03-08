use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use image::GenericImageView;
use serde_json::Value;
use std::path::PathBuf;

pub struct ScreenDiffTool {
    workspace: PathBuf,
    threshold: f32,
}

impl ScreenDiffTool {
    pub fn new(workspace_dir: &str, threshold: f32) -> Self {
        Self {
            workspace: PathBuf::from(workspace_dir),
            threshold,
        }
    }
}

#[async_trait]
impl Tool for ScreenDiffTool {
    fn name(&self) -> &str {
        "screen_diff"
    }

    fn description(&self) -> &str {
        "Compares two screenshots to verify if the UI changed significantly. Use this immediately after taking an action (like a click or typing) to verify success before proceeding to the next step."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "image1": {
                    "type": "string",
                    "description": "Path to the first screenshot (e.g. before action)"
                },
                "image2": {
                    "type": "string",
                    "description": "Path to the second screenshot (e.g. after action)"
                }
            },
            "required": ["image1", "image2"]
        })
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let (img1_path, img2_path) = match (
            input.get("image1").and_then(|v| v.as_str()),
            input.get("image2").and_then(|v| v.as_str()),
        ) {
            (Some(p1), Some(p2)) => (p1, p2),
            _ => anyhow::bail!("Missing 'image1' or 'image2' in input payload"),
        };

        if !img1_path.starts_with("./workspace") || !img2_path.starts_with("./workspace") {
            anyhow::bail!("Access to files outside of workspace bounds is blocked.");
        }

        // Relative base check logic
        let mut resolved_1 = self.workspace.clone();
        resolved_1.push(
            img1_path
                .trim_start_matches("./workspace/")
                .trim_start_matches('/'),
        );

        let mut resolved_2 = self.workspace.clone();
        resolved_2.push(
            img2_path
                .trim_start_matches("./workspace/")
                .trim_start_matches('/'),
        );

        if !resolved_1.exists() {
            anyhow::bail!("Image 1 not found: {}", img1_path);
        }
        if !resolved_2.exists() {
            anyhow::bail!("Image 2 not found: {}", img2_path);
        }

        let img_a = image::open(&resolved_1).context("Failed to decode image 1")?;
        let img_b = image::open(&resolved_2).context("Failed to decode image 2")?;

        if img_a.dimensions() != img_b.dimensions() {
            // Technically different simply based on geometry
            return Ok(serde_json::json!({
                "changed": true,
                "difference_percent": 100.0,
                "note": "Dimensions mismatched, considered entirely changed."
            }));
        }

        let width = img_a.width();
        let height = img_a.height();
        let total_pixels = (width * height) as f32;

        let rgba_a = img_a.to_rgba8();
        let rgba_b = img_b.to_rgba8();

        let mut diff_pixels = 0;

        for (pixel_a, pixel_b) in rgba_a.pixels().zip(rgba_b.pixels()) {
            if pixel_a != pixel_b {
                let r_diff = (pixel_a[0] as i32 - pixel_b[0] as i32).abs();
                let g_diff = (pixel_a[1] as i32 - pixel_b[1] as i32).abs();
                let b_diff = (pixel_a[2] as i32 - pixel_b[2] as i32).abs();
                // We define a pixel as 'changed' if any color channel shifted by > 10 to account for subtle anti-aliasing rendering entropy.
                if r_diff > 10 || g_diff > 10 || b_diff > 10 {
                    diff_pixels += 1;
                }
            }
        }

        let difference_percent = (diff_pixels as f32 / total_pixels) * 100.0;
        let changed = difference_percent >= self.threshold;

        Ok(serde_json::json!({
            "changed": changed,
            "difference_percent": difference_percent,
        }))
    }
}
