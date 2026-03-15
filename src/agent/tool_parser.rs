use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub tool: String,
    pub args: Value,
}

pub fn normalize_tool_name(name: &str) -> String {
    let mut cleaned = name
        .replace("<|tool_call_begin|>", "")
        .replace("<|tool_call_end|>", "")
        .replace("<|tool_call", "")
        .replace("functions.", "");
    if let Some((base, suffix)) = cleaned.rsplit_once(':') {
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            cleaned = base.to_string();
        }
    }
    cleaned.trim().to_string()
}

pub fn resolve_tool_alias(name: &str) -> &str {
    match name {
        "ui_detector" => "detect_ui_elements",
        "detect_ui" => "detect_ui_elements",
        "vision_detect" => "detect_ui_elements",
        "mouse_click" => "mouse",
        other => other,
    }
}

/// Public wrapper for extracting a JSON object from text (used by planner fallback).
pub fn extract_json_object_from(text: &str) -> Option<&str> {
    extract_json_object(text)
}

pub fn parse_tool_call(response: &str) -> Option<ToolCall> {
    parse_tool_calls(response).into_iter().next()
}

pub fn parse_tool_calls(response: &str) -> Vec<ToolCall> {
    tracing::debug!("Raw LLM output: {}", response);

    let sanitized = sanitize_llm_output(response);
    tracing::debug!("Sanitized LLM output: {}", sanitized);

    if sanitized.is_empty() {
        return Vec::new();
    }

    if let Some(calls) = parse_json_tool_calls(sanitized.as_str()) {
        tracing::debug!("Parsed {} tool call(s) from direct JSON", calls.len());
        return calls;
    }

    if let Some(calls) = parse_function_style_calls(sanitized.as_str()) {
        tracing::debug!("Parsed {} tool call(s) from function-style blocks", calls.len());
        return calls;
    }

    // Try extracting JSON from code fences BEFORE line-based parsing
    if let Some(block_json) = extract_json_from_code_fence(sanitized.as_str()) {
        if let Some(calls) = parse_json_tool_calls(&block_json) {
            tracing::debug!("Parsed {} tool call(s) from fenced JSON", calls.len());
            return calls;
        }
    }

    // Try extracting a JSON object embedded anywhere in the text
    // This handles models that prefix tool calls with explanations
    if let Some(obj) = extract_json_object(sanitized.as_str()) {
        if let Some(calls) = parse_json_tool_calls(obj) {
            tracing::debug!("Parsed {} tool call(s) from extracted JSON", calls.len());
            return calls;
        }
    }

    // Last resort: try interpreting first line as tool name + JSON body
    if let Some(call) = parse_line_tool_call(sanitized.as_str()) {
        tracing::debug!("Parsed tool name: {}", call.tool);
        return vec![call];
    }

    Vec::new()
}

pub fn sanitize_llm_output(text: &str) -> String {
    let mut cleaned = text.to_string();
    cleaned = cleaned.replace("<|tool_call_argument_begin|>", "\n");
    for token in [
        "<|start|>",
        "<|channel|>",
        "<|message|>",
        "<|call|>",
        "<|tool_calls_section_begin|>",
        "<|tool_calls_section_end|>",
        "<|tool_call_begin|>",
        "<|tool_call_end|>",
    ] {
        cleaned = cleaned.replace(token, "");
    }
    cleaned.trim().to_string()
}

fn normalize_args_value(value: Value) -> Value {
    match value {
        Value::String(raw) => serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw)),
        other => other,
    }
}

fn parse_json_tool_call(candidate: &str) -> Option<ToolCall> {
    let value: Value = serde_json::from_str(candidate).ok()?;
    parse_json_tool_call_value(&value)
}

fn parse_json_tool_calls(candidate: &str) -> Option<Vec<ToolCall>> {
    let value: Value = serde_json::from_str(candidate).ok()?;
    parse_json_tool_calls_value(&value)
}

fn parse_json_tool_call_value(value: &Value) -> Option<ToolCall> {
    let raw_tool = value
        .get("tool")
        .or_else(|| value.get("tool_name"))
        .and_then(Value::as_str)?
        .trim();
    if raw_tool.is_empty() {
        return None;
    }

    let normalized_tool = resolve_tool_alias(&normalize_tool_name(raw_tool)).to_string();
    if normalized_tool.is_empty() {
        return None;
    }

    let args = value
        .get("args")
        .or_else(|| value.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args = normalize_args_value(args);

    Some(ToolCall {
        tool: normalized_tool.to_string(),
        args: if args.is_null() { json!({}) } else { args },
    })
}

fn parse_tool_call_entry(entry: &Value) -> Option<ToolCall> {
    if let Some(call) = parse_json_tool_call_value(entry) {
        return Some(call);
    }

    let obj = entry.as_object()?;

    if let Some(function) = obj.get("function").and_then(Value::as_object) {
        let name = function.get("name").and_then(Value::as_str)?.trim();
        if name.is_empty() {
            return None;
        }
        let args = function
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let args = normalize_args_value(args);
        let normalized_tool = resolve_tool_alias(&normalize_tool_name(name)).to_string();
        if normalized_tool.is_empty() {
            return None;
        }
        return Some(ToolCall {
            tool: normalized_tool,
            args: if args.is_null() { json!({}) } else { args },
        });
    }

    if let Some(name) = obj.get("name").and_then(Value::as_str) {
        let args = obj
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let args = normalize_args_value(args);
        let normalized_tool = resolve_tool_alias(&normalize_tool_name(name)).to_string();
        if normalized_tool.is_empty() {
            return None;
        }
        return Some(ToolCall {
            tool: normalized_tool,
            args: if args.is_null() { json!({}) } else { args },
        });
    }

    None
}

fn parse_json_tool_calls_value(value: &Value) -> Option<Vec<ToolCall>> {
    match value {
        Value::Array(items) => {
            let calls: Vec<ToolCall> = items
                .iter()
                .filter_map(parse_json_tool_call_value)
                .collect();
            if calls.is_empty() {
                None
            } else {
                Some(calls)
            }
        }
        Value::Object(map) => {
            if let Some(items) = map.get("tool_calls").and_then(Value::as_array) {
                let calls: Vec<ToolCall> = items.iter().filter_map(parse_tool_call_entry).collect();
                if !calls.is_empty() {
                    return Some(calls);
                }
            }
            if let Some(items) = map.get("tools").and_then(Value::as_array) {
                let calls: Vec<ToolCall> = items
                    .iter()
                    .filter_map(parse_tool_call_entry)
                    .collect();
                if calls.is_empty() {
                    None
                } else {
                    Some(calls)
                }
            } else {
                parse_json_tool_call_value(value).map(|call| vec![call])
            }
        }
        _ => None,
    }
}

fn parse_function_style_call(text: &str) -> Option<ToolCall> {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_matches('`').trim_end_matches(':'))?;

    if !first_line.starts_with("functions.") {
        return None;
    }

    let raw_tool_name = first_line;
    let mut body = match text.find(first_line) {
        Some(idx) => text[idx + first_line.len()..].trim(),
        None => "",
    };
    if raw_tool_name.is_empty() {
        body = text.trim();
    }

    let args = if body.is_empty() {
        json!({})
    } else if let Some(obj) = extract_json_object(body) {
        serde_json::from_str::<Value>(obj)
            .ok()
            .unwrap_or_else(|| json!({}))
    } else {
        serde_json::from_str::<Value>(body)
            .ok()
            .unwrap_or_else(|| json!({}))
    };

    let mut tool_name = resolve_tool_alias(&normalize_tool_name(raw_tool_name)).to_string();
    if tool_name.is_empty() {
        if let Some(t) = args.get("tool").and_then(|v| v.as_str()) {
            tool_name = resolve_tool_alias(&normalize_tool_name(t)).to_string();
        }
        if tool_name.is_empty() {
            return None;
        }
    }

    let final_args = args
        .get("args")
        .or_else(|| args.get("arguments"))
        .cloned()
        .unwrap_or(args);
    let final_args = normalize_args_value(final_args);

    Some(ToolCall {
        tool: tool_name,
        args: if final_args.is_null() {
            json!({})
        } else {
            final_args
        },
    })
}

fn parse_function_style_calls(text: &str) -> Option<Vec<ToolCall>> {
    let mut calls = Vec::new();
    let mut cursor = 0usize;

    while let Some(found) = text[cursor..].find("functions.") {
        let start = cursor + found;
        let rest = &text[start..];
        let name_end = rest
            .find(|ch: char| ch.is_whitespace())
            .unwrap_or(rest.len());
        let raw_tool_name = rest[..name_end].trim();
        if raw_tool_name.is_empty() {
            cursor = start + name_end;
            continue;
        }

        let after_name = &rest[name_end..];
        if let Some(obj) = extract_json_object(after_name) {
            if let Ok(value) = serde_json::from_str::<Value>(obj) {
                let tool_name = resolve_tool_alias(&normalize_tool_name(raw_tool_name)).to_string();
                if !tool_name.is_empty() {
                    let final_args = value
                        .get("args")
                        .or_else(|| value.get("arguments"))
                        .cloned()
                        .unwrap_or(value);
                    calls.push(ToolCall {
                        tool: tool_name,
                        args: if final_args.is_null() {
                            json!({})
                        } else {
                            final_args
                        },
                    });
                }
            }
            if let Some(obj_offset) = after_name.find(obj) {
                cursor = start + name_end + obj_offset + obj.len();
            } else {
                cursor = start + name_end;
            }
        } else {
            cursor = start + name_end;
        }
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

fn parse_line_tool_call(text: &str) -> Option<ToolCall> {
    let first_line_raw = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if first_line_raw.starts_with("```") {
        return None;
    }
    let first_line = first_line_raw.trim_matches('`').trim_end_matches(':');

    if first_line.is_empty() || first_line.starts_with('{') || first_line.starts_with("```") {
        return None;
    }
    if first_line.eq_ignore_ascii_case("json") {
        return None;
    }
    // Reject lines that look like natural language, not tool names.
    // Real tool names are short, snake_case identifiers without spaces.
    if first_line.contains(' ') || first_line.len() > 50 {
        return None;
    }

    let raw_tool_name = first_line;
    let mut body = match text.find(first_line_raw) {
        Some(idx) => text[idx + first_line_raw.len()..].trim(),
        None => "",
    };
    if raw_tool_name.is_empty() {
        body = text.trim();
    }

    let args_obj = extract_json_object(body)?;
    let args = serde_json::from_str::<Value>(args_obj).ok()?;

    let mut tool_name = resolve_tool_alias(&normalize_tool_name(raw_tool_name)).to_string();
    if tool_name.is_empty() {
        if let Some(t) = args.get("tool").and_then(|v| v.as_str()) {
            tool_name = resolve_tool_alias(&normalize_tool_name(t)).to_string();
        }
        if tool_name.is_empty() {
            return None;
        }
    }

    let final_args = args
        .get("args")
        .or_else(|| args.get("arguments"))
        .cloned()
        .unwrap_or(args);
    let final_args = normalize_args_value(final_args);

    Some(ToolCall {
        tool: tool_name,
        args: final_args,
    })
}

fn extract_json_from_code_fence(text: &str) -> Option<String> {
    let mut offset = 0usize;
    while let Some(start_rel) = text[offset..].find("```") {
        let block_start = offset + start_rel + 3;
        let block_tail = &text[block_start..];
        let end_rel = block_tail.find("```")?;
        let block_end = block_start + end_rel;
        let raw_block = text[block_start..block_end].trim();
        let block = strip_code_fence_language(raw_block);

        if let Some(obj) = extract_json_object(block) {
            return Some(obj.to_string());
        }
        if serde_json::from_str::<Value>(block).is_ok() {
            return Some(block.to_string());
        }

        offset = block_end + 3;
        if offset >= text.len() {
            break;
        }
    }
    None
}

fn strip_code_fence_language(block: &str) -> &str {
    let trimmed = block.trim();
    if let Some((lang, rest)) = trimmed.split_once('\n') {
        if lang.trim().eq_ignore_ascii_case("json") {
            return rest.trim();
        }
    }
    trimmed
}

fn extract_json_object(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' if depth > 0 => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start {
                        return Some(&text[begin..=idx]);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_tool_name, parse_tool_call, parse_tool_calls, resolve_tool_alias,
        sanitize_llm_output,
    };

    #[test]
    fn parses_direct_json_with_args() {
        let response =
            r#"{"tool":"detect_ui_elements","args":{"image_path":"a.png","hint":"search bar"}}"#;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "detect_ui_elements");
        assert_eq!(call.args["image_path"], "a.png");
    }

    #[test]
    fn parses_json_with_arguments_key() {
        let response = r##"{"tool":"browser","arguments":{"action":"click","selector":"#a"}}"##;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "browser");
        assert_eq!(call.args["action"], "click");
    }

    #[test]
    fn parses_parallel_tool_batch() {
        let response = r#"{"tools":[{"tool":"search","args":{"query":"planner"}},{"tool":"project_map","args":{"directory":"src"}}]}"#;
        let calls = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool, "search");
        assert_eq!(calls[1].tool, "project_map");
    }

    #[test]
    fn parses_function_fallback() {
        let response = r#"functions.detect_ui_elements
{"image_path":"screen.png","hint":"search input"}"#;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "detect_ui_elements");
        assert_eq!(call.args["hint"], "search input");
    }

    #[test]
    fn strips_tool_tokens_and_parses() {
        let response = r#"<|tool_calls_section_begin|>
```json
{"tool":"wait_for","args":{"timeout":2}}
```
<|tool_call_end|>"#;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "wait_for");
        assert_eq!(call.args["timeout"], 2);
    }

    #[test]
    fn normalizes_malformed_tool_name() {
        let response = r#"{"tool":"ui_detector:0<|tool_call","args":{"hint":"search field"}}"#;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "detect_ui_elements");
        assert_eq!(call.args["hint"], "search field");
    }

    #[test]
    fn resolves_aliases() {
        assert_eq!(resolve_tool_alias("ui_detector"), "detect_ui_elements");
        assert_eq!(resolve_tool_alias("mouse_click"), "mouse");
        assert_eq!(resolve_tool_alias("browser"), "browser");
    }

    #[test]
    fn parses_line_tool_call_with_alias_and_suffix() {
        let response = r#"detect_ui:0
{"image_path":"a.png","hint":"search"}"#;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "detect_ui_elements");
        assert_eq!(call.args["image_path"], "a.png");
    }

    #[test]
    fn normalize_tool_name_cleans_tokens() {
        let raw = "functions.ui_detector:0<|tool_call_begin|>";
        let cleaned = normalize_tool_name(raw);
        assert_eq!(cleaned, "ui_detector");
    }

    #[test]
    fn normalize_tool_name_strips_any_suffix() {
        let raw = "functions.edit:12";
        let cleaned = normalize_tool_name(raw);
        assert_eq!(cleaned, "edit");
    }

    #[test]
    fn sanitize_llm_output_removes_wrapper_tokens() {
        let raw = "<|start|><|channel|><|message|><|call|>{\"tool\":\"wait_for\",\"args\":{\"timeout\":2}}";
        let sanitized = sanitize_llm_output(raw);
        assert_eq!(
            sanitized,
            "{\"tool\":\"wait_for\",\"args\":{\"timeout\":2}}"
        );
    }

    #[test]
    fn parses_tokenized_function_blocks() {
        let response = r#"I'll create files now.
<|tool_calls_section_begin|>
<|tool_call_begin|> functions.edit:0 <|tool_call_argument_begin|> {"file":"a.txt","action":"create","content":"hello"} <|tool_call_end|>
<|tool_call_begin|> functions.shell:1 <|tool_call_argument_begin|> {"command":"ls"} <|tool_call_end|>
<|tool_calls_section_end|>"#;
        let calls = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool, "edit");
        assert_eq!(calls[0].args["file"], "a.txt");
        assert_eq!(calls[1].tool, "shell");
        assert_eq!(calls[1].args["command"], "ls");
    }

    #[test]
    fn parses_stringified_arguments() {
        let response = r#"{"tool":"shell","args":"{\"command\":\"ls\"}"}"#;
        let call = parse_tool_call(response).expect("expected tool call");
        assert_eq!(call.tool, "shell");
        assert_eq!(call.args["command"], "ls");
    }

    #[test]
    fn parses_openai_tool_calls() {
        let response = r#"{"tool_calls":[{"type":"function","function":{"name":"shell","arguments":"{\"command\":\"ls\"}"}}]}"#;
        let calls = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "shell");
        assert_eq!(calls[0].args["command"], "ls");
    }

    #[test]
    fn parses_named_tool_calls() {
        let response = r#"{"tool_calls":[{"name":"search","arguments":{"query":"planner"}}]}"#;
        let calls = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "search");
        assert_eq!(calls[0].args["query"], "planner");
    }
}
