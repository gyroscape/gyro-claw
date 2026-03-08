use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use headless_chrome::{Browser, LaunchOptions};
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::BrowserConfig;
use crate::llm::client::LlmClient;
use crate::tools::computer::ui_detector::UiDetectorTool;
use crate::tools::Tool;

struct BrowserState {
    navigation_depth: usize,
    action_count: usize,
    max_navigation_depth: usize,
    max_actions: usize,
}

pub struct BrowserTool {
    state: Arc<Mutex<BrowserState>>,
    config: BrowserConfig,
    workspace_root: String,
    llm: LlmClient,
}

impl BrowserTool {
    pub fn new(
        max_requests: usize,
        config: BrowserConfig,
        workspace_root: String,
        llm: LlmClient,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(BrowserState {
                navigation_depth: 0,
                action_count: 0,
                max_navigation_depth: 10,
                max_actions: max_requests,
            })),
            config,
            workspace_root,
            llm,
        }
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Automate Headless Chrome to interact with the internet.
        - 'navigate': url (Navigate to a page)
        - 'wait_for': selector, timeout (Wait for element)
        - 'click': selector (Wait and click)
        - 'type': selector, text (Wait and type)
        - 'extract': selector (Get text/links/title, falls back to readable page content)
        - 'extract_text': (Get readable page text without selectors)
        - 'screenshot': (Capture viewport)
        Returns structured results or JSON errors if elements are missing.
        PREFER VISION-BASED WORKFLOW: screenshot -> detect_ui_elements -> mouse.click instead of relying solely on DOM selectors."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The action to perform",
                    "enum": ["navigate", "wait_for", "click", "type", "extract", "extract_text", "screenshot"]
                },
                "url": { "type": "string", "description": "URL for navigate" },
                "selector": { "type": "string", "description": "CSS selector" },
                "text": { "type": "string", "description": "Text for typing" },
                "timeout": { "type": "integer", "description": "Seconds to wait (optional)" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Safety Check
        {
            let mut s = self
                .state
                .lock()
                .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
            if action == "navigate" {
                if s.navigation_depth >= s.max_navigation_depth {
                    anyhow::bail!("SAFETY LIMIT: Max navigations exceeded.");
                }
                s.navigation_depth += 1;
            } else {
                if s.action_count >= s.max_actions {
                    anyhow::bail!("SAFETY LIMIT: Max actions exceeded.");
                }
                s.action_count += 1;
            }
        }

        let input_clone = input.clone();
        let config_clone = self.config.clone();
        let workspace_root = self.workspace_root.clone();
        let llm_clone = self.llm.clone();
        let runtime_handle = tokio::runtime::Handle::current();

        let result = tokio::task::spawn_blocking(move || {
            let browser = Browser::new(
                LaunchOptions::default_builder()
                    .window_size(Some((1280, 800)))
                    .idle_browser_timeout(Duration::from_secs(30))
                    .build()
                    .unwrap(),
            ).context("Failed to launch browser")?;

            let tab = browser.new_tab().context("Failed to open tab")?;
            let default_timeout = Duration::from_secs(config_clone.default_wait_timeout);
            tab.set_default_timeout(default_timeout);
            let _ = tab.bring_to_front();

            match action.as_str() {
                "navigate" => {
                    let url = input_clone.get("url").and_then(|v| v.as_str()).context("Missing url")?;
                    tab.navigate_to(url)?;
                    tab.wait_until_navigated()?;
                    let _ = tab.bring_to_front();
                    std::thread::sleep(Duration::from_millis(500));
                    std::thread::sleep(Duration::from_secs(5));
                    Ok(json!({"status": "navigated", "url": url, "waited_for_page_load_seconds": 5}))
                }
                "wait_for" => {
                    let raw_selector = input_clone.get("selector").and_then(|v| v.as_str()).context("Missing selector")?;
                    let timeout = input_clone.get("timeout").and_then(|v| v.as_u64()).map(Duration::from_secs).unwrap_or_else(|| Duration::from_secs(3));
                    let selectors = get_fallback_selectors(raw_selector);

                    let mut found = false;
                    let mut successful_selector = String::new();

                    for sel in &selectors {
                        println!("Trying selector: {}", sel);
                        if tab.wait_for_element_with_custom_timeout(sel, timeout).is_ok() {
                            found = true;
                            successful_selector = sel.to_string();
                            break;
                        }
                    }

                    if !found {
                        return Ok(tool_error(
                            "browser",
                            "element_not_found",
                            format!("element not found for selector '{}' or its fallbacks", raw_selector),
                            "Use screenshot/extract and verify selector before retrying.",
                        ));
                    }
                    Ok(json!({"status": "found", "selector": successful_selector}))
                }
                "click" => {
                    let raw_selector = input_clone.get("selector").and_then(|v| v.as_str()).context("Missing selector")?;
                    let selectors = get_fallback_selectors(raw_selector);
                    let timeout = Duration::from_secs(3);

                    let mut clicked_selector = None;

                    for sel in &selectors {
                        println!("Trying selector: {}", sel);
                        if let Ok(element) = tab.wait_for_element_with_custom_timeout(sel, timeout) {
                            if element.click().is_ok() {
                                clicked_selector = Some(sel.to_string());
                                break;
                            }
                        }
                    }

                    let selector = match clicked_selector {
                        Some(s) => s,
                        None => {
                            return Ok(tool_error(
                                "browser",
                                "element_not_found",
                                format!("click target not found using '{}' or fallbacks", raw_selector),
                                "Call wait_for/extract/screenshot to confirm DOM state first.",
                            ))
                        }
                    };
                    Ok(json!({"status": "clicked", "selector": selector}))
                }
                "type" => {
                    let raw_selector = input_clone
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .context("Missing selector")?;
                    let text = input_clone.get("text").and_then(|v| v.as_str()).context("Missing text")?;
                    let selectors = get_fallback_selectors(raw_selector);
                    let timeout = Duration::from_secs(3);

                    let mut type_result = None;

                    for sel in &selectors {
                        println!("Trying selector: {}", sel);
                        // 1) Try direct type.
                        if let Ok(element) = tab.wait_for_element_with_custom_timeout(sel, timeout) {
                            if element.type_into(text).is_ok() {
                                type_result = Some(json!({"status": "typed", "selector": sel, "strategy": "direct_type"}));
                                break;
                            }

                            // 2) If type failed, try focus(selector) then type again.
                            let _ = element.focus();
                            if let Ok(refocused) = tab.wait_for_element_with_custom_timeout(sel, timeout) {
                                if refocused.type_into(text).is_ok() {
                                    type_result = Some(json!({"status": "typed", "selector": sel, "strategy": "focus_then_type"}));
                                    break;
                                }

                                // 3) If still failing, click(selector) then retry type.
                                let _ = refocused.click();
                                if let Ok(recov) = tab.wait_for_element_with_custom_timeout(sel, timeout) {
                                    if recov.type_into(text).is_ok() {
                                        type_result = Some(json!({"status": "typed", "selector": sel, "strategy": "click_then_type"}));
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    if let Some(res) = type_result {
                        return Ok(res);
                    }

                    // 4) Vision fallback: detect_ui_elements -> bbox center click -> keyboard-like type.
                    let _ = tab.bring_to_front();
                    std::thread::sleep(Duration::from_millis(500));
                    let png_data = tab.capture_screenshot(
                        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                        None,
                        None,
                        true,
                    )?;
                    let screenshot_b64 = general_purpose::STANDARD.encode(&png_data);
                    let ui_detector = UiDetectorTool::new(&workspace_root, llm_clone.clone());
                    let hint = if raw_selector.contains("search") || raw_selector == "input" {
                        "search input field"
                    } else {
                        raw_selector
                    };

                    let detection = runtime_handle.block_on(ui_detector.execute(json!({
                        "image_base64": screenshot_b64,
                        "hint": hint
                    })));

                    let detection_value = match detection {
                        Ok(v) => v,
                        Err(err) => {
                            return Ok(tool_error(
                                "browser",
                                "vision_fallback_failed",
                                format!("detect_ui_elements call failed: {}", err),
                                "Capture a new screenshot and retry with a clearer selector.",
                            ));
                        }
                    };

                    if detection_value
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| s == "error")
                        .unwrap_or(false)
                    {
                        return Ok(tool_error(
                            "browser",
                            "element_not_found",
                            format!("type target not found using '{}' and vision fallback failed", raw_selector),
                            "vision fallback could not find element; try a clearer hint.",
                        ));
                    }

                    let (center_x, center_y, bbox_json) = match extract_bbox_center(&detection_value) {
                        Some(v) => v,
                        None => {
                            return Ok(tool_error(
                                "browser",
                                "vision_fallback_failed",
                                "detect_ui_elements returned no usable bounding box".to_string(),
                                "Retry with a clearer selector or capture a newer screenshot.",
                            ));
                        }
                    };

                    tab.click_point(headless_chrome::browser::tab::point::Point {
                        x: center_x,
                        y: center_y,
                    })?;
                    tab.type_str(text)?.press_key("Enter")?;

                    Ok(json!({
                        "status": "typed",
                        "selector": raw_selector,
                        "strategy": "vision_bbox_click_then_keyboard_type_and_enter",
                        "bounding_box": bbox_json,
                        "center_x": center_x,
                        "center_y": center_y
                    }))
                }
                "extract" => {
                    let title = tab.get_title().unwrap_or_default();
                    let raw_selector = input_clone
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .unwrap_or("p");
                    let selectors = get_extract_fallback_selectors(raw_selector);
                    let timeout = Duration::from_secs(3);

                    let mut extracted_text = String::new();
                    let mut matched_selector = None;
                    for sel in &selectors {
                        println!("Trying extract selector: {}", sel);
                        if let Ok(el) = tab.wait_for_element_with_custom_timeout(sel, timeout) {
                            let text = el.get_inner_text().unwrap_or_default().trim().to_string();
                            if !text.is_empty() {
                                extracted_text = text;
                                matched_selector = Some(sel.to_string());
                                break;
                            }
                        }
                    }

                    if extracted_text.is_empty() {
                        extracted_text = tab
                            .evaluate("document.body ? document.body.innerText : ''", false)
                            .ok()
                            .and_then(|value| value.value)
                            .and_then(|value| value.as_str().map(|s| s.trim().to_string()))
                            .unwrap_or_default();
                        if !extracted_text.is_empty() {
                            matched_selector = Some("document.body".to_string());
                        }
                    }

                    if extracted_text.is_empty() {
                        return Ok(tool_error(
                            "browser",
                            "extract_failed",
                            format!(
                                "extract target not found using '{}' or readable page fallbacks",
                                raw_selector
                            ),
                            "Use extract_text or capture a screenshot to verify page content first.",
                        ));
                    }

                    // Basic link extraction via evaluate
                    let links_val = tab.evaluate("Array.from(document.querySelectorAll('a')).map(a => a.href)", false).ok();
                    let links: Vec<String> = if let Some(lv) = links_val {
                       lv.value.and_then(|v| serde_json::from_value::<Vec<String>>(v).ok()).unwrap_or_default()
                    } else { vec![] };

                    let truncated_text = if extracted_text.len() > 10000 {
                        format!("{}... [TRUNCATED]", &extracted_text[..10000])
                    } else {
                        extracted_text
                    };
                    Ok(json!({
                        "status": "extracted",
                        "title": title,
                        "selector": matched_selector.unwrap_or_else(|| raw_selector.to_string()),
                        "text": truncated_text,
                        "links": links.into_iter().take(50).collect::<Vec<_>>()
                    }))
                }
                "extract_text" => {
                    let title = tab.get_title().unwrap_or_default();
                    let text = tab
                        .evaluate("document.body ? document.body.innerText : ''", false)
                        .ok()
                        .and_then(|value| value.value)
                        .and_then(|value| value.as_str().map(|s| s.trim().to_string()))
                        .unwrap_or_default();

                    if text.is_empty() {
                        return Ok(tool_error(
                            "browser",
                            "extract_failed",
                            "page did not contain readable text".to_string(),
                            "Navigate to the destination page first, then retry extract_text.",
                        ));
                    }

                    let truncated_text = if text.len() > 10000 {
                        format!("{}... [TRUNCATED]", &text[..10000])
                    } else {
                        text
                    };

                    Ok(json!({
                        "status": "extracted",
                        "title": title,
                        "selector": "document.body",
                        "text": truncated_text
                    }))
                }
                "screenshot" => {
                    let _ = tab.bring_to_front();
                    std::thread::sleep(Duration::from_millis(500));
                    let png_data = tab.capture_screenshot(headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png, None, None, true)?;
                    let b64 = general_purpose::STANDARD.encode(&png_data);
                    let screenshots_dir = std::path::Path::new(&workspace_root).join("screenshots");
                    std::fs::create_dir_all(&screenshots_dir)?;
                    let filepath = screenshots_dir.join("browser_screenshot.png");
                    let mut file = std::fs::File::create(&filepath)?;
                    file.write_all(&png_data)?;
                    file.flush()?;
                    Ok(json!({
                        "image_path": filepath.to_string_lossy().to_string(),
                        "image_base64": b64
                    }))
                }
                _ => anyhow::bail!("Unknown action"),
            }
        })
        .await
        .context("Blocking task failed")??;

        Ok(result)
    }
}

fn tool_error(
    tool: &str,
    error_type: &str,
    message: String,
    suggestion: &str,
) -> serde_json::Value {
    json!({
        "status": "error",
        "tool": tool,
        "error_type": error_type,
        "message": message,
        "suggestion": suggestion
    })
}

fn get_fallback_selectors(selector: &str) -> Vec<String> {
    let trimmed = selector.trim();
    let mut selectors = vec![trimmed.to_string()];

    let is_search = trimmed.to_lowercase().contains("search")
        || trimmed == "input"
        || trimmed.contains("query");

    if is_search {
        selectors.push("#search-input input".to_string());
        selectors.push("input[name='search_query']".to_string());
        selectors.push("input[placeholder*='Search']".to_string());
        selectors.push("input[name='q']".to_string());
        selectors.push("input[type='text']".to_string());
        selectors.push("input#search".to_string());
    }

    selectors
}

fn get_extract_fallback_selectors(selector: &str) -> Vec<String> {
    let mut selectors = get_fallback_selectors(selector);

    for fallback in [
        "article",
        "main",
        "[role='main']",
        ".content",
        ".post",
        ".article",
        "body",
        "p",
        "a",
    ] {
        if !selectors.iter().any(|existing| existing == fallback) {
            selectors.push(fallback.to_string());
        }
    }

    selectors
}

fn extract_bbox_center(value: &serde_json::Value) -> Option<(f64, f64, serde_json::Value)> {
    let bbox = value
        .get("result")
        .or_else(|| value.get("bounding_box"))
        .or_else(|| {
            value
                .get("elements")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
        })?;

    let x = bbox.get("x").and_then(as_f64)?;
    let y = bbox.get("y").and_then(as_f64)?;
    let width = bbox.get("width").and_then(as_f64).unwrap_or(1.0).max(1.0);
    let height = bbox.get("height").and_then(as_f64).unwrap_or(1.0).max(1.0);
    let center_x = bbox
        .get("center_x")
        .and_then(as_f64)
        .unwrap_or(x + width / 2.0);
    let center_y = bbox
        .get("center_y")
        .and_then(as_f64)
        .unwrap_or(y + height / 2.0);

    Some((center_x, center_y, bbox.clone()))
}

fn as_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}
