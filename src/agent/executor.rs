//! # Secure Executor
//!
//! The execution layer between the AI model and tools.
//! The AI model NEVER executes tools directly — the executor mediates all calls.
//!
//! Responsibilities:
//! - Load tool permission policy from config (allow / ask / deny)
//! - Validate tool inputs
//! - Block dangerous commands (rm -rf /, shutdown, format, mkfs, etc.)
//! - Sandbox shell commands to an allowlist of safe commands
//! - Load secrets from vault ONLY for allowed tools (scoped injection)
//! - Enforce per-session tool call limits
//! - Run the tool safely

use anyhow::{bail, Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use rand::{rngs::OsRng, RngCore};
use jsonschema::{Draft, JSONSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use zeroize::Zeroize;

use crate::agent::planner::ToolCall;
use crate::agent::tool_parser::{normalize_tool_name, resolve_tool_alias};
use crate::config::{Config, SecretToolPolicy, ToolPermission};
use crate::tools::{ToolRegistry, ToolSecretPolicy};
use crate::vault::secrets::{SecretRecord, SecretVault};
use crate::vault::telemetry::{
    hmac_fingerprint, AnomalyDetector, SecretAccessEvent, SecretRateLimiter, VaultSession,
};

/// List of dangerous shell command patterns that are ALWAYS blocked.
const BLOCKED_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    "rm -rf ~/*",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init 0",
    "init 6",
    "mkfs",
    "format",
    "dd if=/dev/zero",
    "dd if=/dev/random",
    ":(){:|:&};:",    // fork bomb
    "chmod -R 777 /", // recursive permission change on root
    "chown -R",
    "wget | sh",
    "curl | sh",
    "wget | bash",
    "curl | bash",
];

/// Shell commands allowed in sandboxed mode.
const ALLOWED_SHELL_COMMANDS: &[&str] = &[
    "ls", "cat", "echo", "grep", "pwd", "find", "head", "tail", "wc", "sort", "uniq", "diff",
    "date", "whoami", "env", "which", "file", "mkdir", "cp", "mv", "touch", "tree", "du", "df",
    "uname", "curl", "wget", "ping", "dig", "nslookup", "git", "cargo", "rustc", "npm", "node",
    "python", "python3", "pip", "cd", "npx", "yarn", "pnpm", "bun",
];

const REDACTED_VALUE: &str = "[REDACTED_SECRET]";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_name: String,
    pub arguments: Value,
    pub output: Value,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct SecretExposure {
    key: String,
    value: String,
    fingerprint: String,
    token_fingerprints: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct SecretTaintContext {
    exposures: Vec<SecretExposure>,
    contains_secret_input: bool,
}

impl SecretTaintContext {
    fn add(&mut self, record: &SecretRecord) {
        self.contains_secret_input = true;
        self.exposures.push(SecretExposure {
            key: record.key.clone(),
            value: record.value.clone(),
            fingerprint: record.fingerprint.clone(),
            token_fingerprints: record.token_fingerprints.clone(),
        });
    }

    fn wipe(&mut self) {
        for exposure in &mut self.exposures {
            Executor::wipe_string(&mut exposure.value);
            Executor::wipe_string(&mut exposure.fingerprint);
            for token_fp in &mut exposure.token_fingerprints {
                Executor::wipe_string(token_fp);
            }
            exposure.token_fingerprints.clear();
            Executor::wipe_string(&mut exposure.key);
        }
        self.exposures.clear();
        self.contains_secret_input = false;
    }
}

/// The Executor validates, protects, and runs tool calls.
pub struct Executor {
    vault: Option<Arc<SecretVault>>,
    config: Config,
    /// Whether to enforce shell command sandboxing
    sandbox_shell: bool,
    /// Counter for total tool calls in this session
    tool_call_count: AtomicUsize,
    /// Per-task / per-minute secret resolution rate limiter
    rate_limiter: SecretRateLimiter,
    /// Time-limited vault unlock session
    vault_session: Arc<RwLock<VaultSession>>,
    /// Anomaly detection engine
    anomaly_detector: AnomalyDetector,
    /// Unique identifier for this executor instance (for telemetry)
    executor_instance_id: String,
}

impl Executor {
    /// Create a new executor with config and optional vault.
    pub fn new(vault: Option<Arc<SecretVault>>, config: Config) -> Self {
        let rate_limiter = SecretRateLimiter::new(
            config.secrets.max_secret_resolutions_per_task,
            config.secrets.max_secret_resolutions_per_minute,
        );
        let vault_session = Arc::new(RwLock::new(VaultSession::new(
            config.secrets.vault_session_duration_seconds,
        )));
        let anomaly_detector = AnomalyDetector::new(config.secrets.anomaly_alert_threshold);
        let executor_instance_id = uuid::Uuid::new_v4().to_string();

        // Auto-unlock vault session if vault is available
        if vault.is_some() {
            if let Ok(mut session) = vault_session.write() {
                session.unlock();
            }
        }

        Self {
            vault,
            config,
            sandbox_shell: true,
            tool_call_count: AtomicUsize::new(0),
            rate_limiter,
            vault_session,
            anomaly_detector,
            executor_instance_id,
        }
    }

    /// Set whether shell commands are sandboxed to the allowlist.
    pub fn set_sandbox_shell(&mut self, sandbox: bool) {
        self.sandbox_shell = sandbox;
    }

    /// Execute a tool call securely.
    pub async fn execute(
        &self,
        tool_name: &str,
        mut input: Value,
        registry: &ToolRegistry,
    ) -> Result<Value> {
        let raw_tool_name = tool_name.trim().to_string();
        let normalized_tool_name =
            resolve_tool_alias(&normalize_tool_name(&raw_tool_name)).to_string();

        tracing::info!(
            tool = %raw_tool_name,
            normalized_tool = %normalized_tool_name,
            "planner requested tool"
        );

        let tool_name = normalized_tool_name.as_str();

        // Step 1: Enforce tool call limit
        let count = self.tool_call_count.fetch_add(1, Ordering::SeqCst);
        if count >= self.config.max_tool_calls {
            return Ok(Self::error_response(
                "planner",
                "tool_call_limit",
                format!(
                    "tool call limit reached ({}/{})",
                    count + 1,
                    self.config.max_tool_calls
                ),
                "Start a fresh run or reduce repeated tool calls.",
            ));
        }

        // Step 2: Validate tool exists
        let tool = match registry.get(tool_name) {
            Some(tool) => tool,
            None => {
                let suggestions = registry.suggest_tools(tool_name, 3);
                return Ok(json!({
                    "status": "error",
                    "error_type": "unknown_tool",
                    "requested_tool": raw_tool_name,
                    "normalized_tool": normalized_tool_name,
                    "message": format!("unknown tool after normalization: '{}'", tool_name),
                    "suggestion": "Use a valid registered tool name or alias.",
                    "did_you_mean": suggestions
                }));
            }
        };
        let tool_secret_policy = tool.secret_policy();

        // Normalize common argument aliases for better tool calling.
        self.normalize_tool_arguments(tool_name, &mut input);

        // Normalize bounding-box click inputs to center-point coordinates.
        if tool_name == "mouse" {
            self.normalize_mouse_click_target(&mut input);
        }

        // Step 3: Optional Sandbox Path Auto-Correction
        if tool_name == "filesystem" || tool_name == "edit" || tool_name == "search" {
            let path_key = if input.get("path").is_some() {
                "path"
            } else if input.get("file").is_some() {
                "file"
            } else {
                "dir"
            };

            if let Some(path_str) = input.get(path_key).and_then(|v| v.as_str()) {
                // If the path is just a simple filename/directory without traversal/root and missing workspace
                if !path_str.starts_with('/')
                    && !path_str.contains("..")
                    && !path_str.starts_with("./workspace")
                    && !path_str.starts_with("workspace/")
                {
                    input[path_key] =
                        serde_json::Value::String(format!("./workspace/{}", path_str));
                }
            }
        }

        // Step 4: Validate tool arguments against the declared JSON schema.
        if let Some(error_response) = self.validate_tool_input(tool, &input) {
            return Ok(error_response);
        }

        // Step 5: Check permission policy (allow / ask / deny)
        if let Err(err) = self.check_permission(tool_name, &input) {
            return Ok(Self::error_response(
                tool_name,
                "permission_denied",
                err.to_string(),
                "Switch to autonomous mode or update tool permissions in config.",
            ));
        }

        // Step 6: Block dangerous commands
        if let Err(err) = self.validate_safety(tool_name, &input) {
            return Ok(Self::error_response(
                tool_name,
                "safety_blocked",
                err.to_string(),
                "Use a safer command or adjust the requested action.",
            ));
        }

        // Step 7: Sandbox shell commands
        if self.sandbox_shell && tool_name == "shell" {
            if let Err(err) = self.validate_shell_sandbox(&input) {
                return Ok(Self::error_response(
                    tool_name,
                    "sandbox_violation",
                    err.to_string(),
                    "Use only commands in the allowed sandbox list.",
                ));
            }
        }

        // Step 8: Resolve secrets under strict zero-trust policy.
        let mut taint = SecretTaintContext::default();
        if self.contains_vault_refs(&input) {
            if let Err(err) =
                self.inject_secrets(tool_name, &tool_secret_policy, &mut input, &mut taint)
            {
                self.anomaly_detector.record_policy_violation();
                tracing::warn!(
                    tool = tool_name,
                    error = %err,
                    "secret policy blocked tool execution"
                );
                return Ok(Self::error_response(
                    tool_name,
                    "secret_policy_violation",
                    err.to_string(),
                    "Use an authorized tool/key scope and keep unresolved placeholders in planner output.",
                ));
            }
        }

        // Step 7: Execute the tool with a strict timeout
        // Suppress debug logging when vault session is active to prevent leaking secrets.
        let vault_active = self
            .vault_session
            .read()
            .map(|s| s.is_active())
            .unwrap_or(false);
        if !vault_active {
            let mut loggable_input = input.clone();
            self.redact_with_taint(&mut loggable_input, &taint);
            tracing::debug!(
                tool = tool_name,
                call_index = count + 1,
                max_tool_calls = self.config.max_tool_calls,
                input = %Self::loggable_json(&loggable_input),
                "executor running tool"
            );
        } else {
            tracing::debug!(tool = tool_name, "[debug suppressed — vault active]");
        }

        let mut timeout_duration = Duration::from_secs(self.config.execution.tool_timeout_seconds);
        if let Some(custom) = input.get("timeout_secs") {
            let parsed_timeout = if let Some(n) = custom.as_u64() {
                Some(n)
            } else if let Some(n) = custom.as_f64() {
                Some(n as u64)
            } else if let Some(s) = custom.as_str() {
                s.parse::<u64>().ok()
            } else {
                None
            };
            if let Some(t) = parsed_timeout {
                timeout_duration = Duration::from_secs(t.clamp(1, 300));
            }
        }
        let execution_result =
            tokio::time::timeout(timeout_duration, tool.execute(input.clone())).await;
        let output = match execution_result {
            Ok(Ok(val)) => {
                let mut normalized = Self::normalize_tool_output(tool_name, val);

                // Step 8: Redact any accidentally leaked secrets from the output.
                // Normalize output before redaction scanning to defeat formatting attacks.
                self.normalize_and_redact_secrets(&mut normalized, &taint);

                if !vault_active {
                    tracing::debug!(
                        tool = tool_name,
                        output = %Self::loggable_json(&normalized),
                        "tool execution completed"
                    );
                }
                normalized
            }
            Ok(Err(err)) => {
                let redacted_error = self.redact_text_with_taint(&err.to_string(), &taint);
                tracing::warn!(
                    tool = tool_name,
                    error = %redacted_error,
                    "tool execution failed"
                );
                Self::error_response(
                    tool_name,
                    "tool_execution_failed",
                    redacted_error,
                    "Inspect arguments or collect more context before retrying.",
                )
            }
            Err(_) => {
                tracing::warn!(
                    tool = tool_name,
                    timeout_secs = timeout_duration.as_secs(),
                    "tool execution timed out"
                );
                json!({
                    "status": "error",
                    "error_type": "timeout",
                    "tool": tool_name,
                    "message": "tool execution exceeded timeout",
                    "suggestion": "Retry with narrower scope or simpler action."
                })
            }
        };

        // Step 9: Ephemeral cleanup of local secret-bearing buffers.
        self.scrub_tainted_value(&mut input, &taint);
        taint.wipe();

        Ok(output)
    }

    pub fn is_parallel_tool_call_safe(&self, call: &ToolCall, registry: &ToolRegistry) -> bool {
        let normalized = normalize_tool_name(&call.tool_name);
        let tool_name = resolve_tool_alias(&normalized);
        let Some(tool) = registry.get(tool_name) else {
            return false;
        };

        if tool.is_parallel_safe() {
            return true;
        }

        match tool_name {
            "filesystem" => call.arguments.get("action").and_then(|v| v.as_str()) == Some("list"),
            "git" => matches!(
                call.arguments.get("command").and_then(|v| v.as_str()),
                Some("status" | "diff" | "log")
            ),
            "test_runner" => matches!(
                call.arguments.get("command").and_then(|v| v.as_str()),
                Some("test" | "check" | "build" | "clippy")
            ),
            _ => false,
        }
    }

    pub async fn execute_parallel_tools(
        &self,
        tools: Vec<ToolCall>,
        registry: &ToolRegistry,
    ) -> Vec<ToolResult> {
        let mut pending = FuturesUnordered::new();
        let total = tools.len();

        for (index, call) in tools.into_iter().enumerate() {
            let is_parallel_safe = self.is_parallel_tool_call_safe(&call, registry);

            pending.push(async move {
                if !is_parallel_safe {
                    let error_output = Self::error_response(
                        &call.tool_name,
                        "parallel_not_safe",
                        "tool call is not safe to run in parallel",
                        "Run mutating tools sequentially.",
                    );
                    return (
                        index,
                        ToolResult {
                            tool_name: call.tool_name,
                            arguments: call.arguments,
                            output: error_output,
                            success: false,
                            error: Some("tool call is not safe to run in parallel".to_string()),
                        },
                    );
                }

                let result = self
                    .execute(&call.tool_name, call.arguments.clone(), registry)
                    .await;
                let tool_result = match result {
                    Ok(output) => ToolResult {
                        tool_name: call.tool_name,
                        arguments: call.arguments,
                        success: !Self::value_is_error(&output),
                        error: Self::extract_error_message(&output),
                        output,
                    },
                    Err(err) => ToolResult {
                        tool_name: call.tool_name,
                        arguments: call.arguments,
                        output: Self::error_response(
                            "planner",
                            "parallel_execution_failed",
                            err.to_string(),
                            "Inspect the tool inputs and retry sequentially if needed.",
                        ),
                        success: false,
                        error: Some(err.to_string()),
                    },
                };
                (index, tool_result)
            });
        }

        let mut ordered: Vec<Option<ToolResult>> = vec![None; total];
        while let Some((index, result)) = pending.next().await {
            ordered[index] = Some(result);
        }

        ordered.into_iter().flatten().collect()
    }

    /// Check the permission policy for a tool.
    /// - Allow: run immediately
    /// - Ask: prompt user for confirmation
    /// - Deny: block execution
    fn check_permission(&self, tool_name: &str, input: &Value) -> Result<()> {
        let permission = self.config.get_tool_permission(tool_name);

        match permission {
            ToolPermission::Allow => {
                // Auto-approved by policy
                Ok(())
            }
            ToolPermission::Ask => {
                // In autonomous mode, ask-level tools auto-approve with no prompt.
                if self.config.is_autonomous() {
                    Ok(())
                } else {
                    self.prompt_user_confirmation(tool_name, input)
                }
            }
            ToolPermission::Deny => {
                bail!(
                    "🚫 Tool '{}' is denied by policy. \
                     Update ~/.gyro-claw/config.toml to change the permission.",
                    tool_name
                );
            }
        }
    }

    /// Prompt the user to confirm a tool execution.
    fn prompt_user_confirmation(&self, tool_name: &str, input: &Value) -> Result<()> {
        let input_preview = match tool_name {
            "shell" => input
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("<unknown>")
                .to_string(),
            "filesystem" | "edit" => {
                let action = input.get("action").and_then(|a| a.as_str()).unwrap_or("?");
                let path = input
                    .get("path")
                    .or_else(|| input.get("file"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("?");
                format!("{} {}", action, path)
            }
            "http" => {
                let method = input.get("method").and_then(|m| m.as_str()).unwrap_or("?");
                let url = input.get("url").and_then(|u| u.as_str()).unwrap_or("?");
                format!("{} {}", method, url)
            }
            "git" => {
                let cmd = input.get("command").and_then(|c| c.as_str()).unwrap_or("?");
                format!("git {}", cmd)
            }
            "search" | "web_search" => {
                let query = input.get("query").and_then(|q| q.as_str()).unwrap_or("?");
                query.to_string()
            }
            "test_runner" => {
                let cmd = input.get("command").and_then(|c| c.as_str()).unwrap_or("?");
                format!("cargo {}", cmd)
            }
            _ => serde_json::to_string(input).unwrap_or_default(),
        };

        eprintln!("\n┌─────────────────────────────────────────");
        eprintln!("│ 🔧 Tool: {}", tool_name);
        eprintln!("│ 📋 Action: {}", input_preview);
        eprintln!("└─────────────────────────────────────────");
        eprint!("  Allow this tool execution? [y/n]: ");
        std::io::stderr().flush().ok();

        let mut response = String::new();
        std::io::stdin()
            .read_line(&mut response)
            .context("Failed to read user input")?;

        let response = response.trim().to_lowercase();
        if response != "y" && response != "yes" {
            bail!("❌ Tool execution denied by user.");
        }

        Ok(())
    }

    fn normalize_mouse_click_target(&self, input: &mut Value) {
        let mut x = input.get("x").and_then(|v| v.as_i64());
        let mut y = input.get("y").and_then(|v| v.as_i64());
        let mut width = input.get("width").and_then(|v| v.as_i64());
        let mut height = input.get("height").and_then(|v| v.as_i64());

        if let Some(bb) = input.get("bounding_box").and_then(|v| v.as_object()) {
            x = x.or_else(|| bb.get("x").and_then(|v| v.as_i64()));
            y = y.or_else(|| bb.get("y").and_then(|v| v.as_i64()));
            width = width.or_else(|| bb.get("width").and_then(|v| v.as_i64()));
            height = height.or_else(|| bb.get("height").and_then(|v| v.as_i64()));
        }

        if let (Some(left), Some(top), Some(w), Some(h)) = (x, y, width, height) {
            if w > 0 && h > 0 {
                input["x"] = Value::Number((left + w / 2).into());
                input["y"] = Value::Number((top + h / 2).into());
            }
        }
    }

    fn normalize_tool_arguments(&self, tool_name: &str, input: &mut Value) {
        let Some(map) = input.as_object_mut() else {
            return;
        };

        fn move_alias(map: &mut serde_json::Map<String, Value>, from: &str, to: &str) {
            if map.contains_key(to) {
                return;
            }
            if let Some(value) = map.remove(from) {
                map.insert(to.to_string(), value);
            }
        }

        match tool_name {
            "edit" => {
                move_alias(map, "path", "file");
                move_alias(map, "filepath", "file");
            }
            "filesystem" => {
                move_alias(map, "file", "path");
                move_alias(map, "filepath", "path");
            }
            "shell" => {
                move_alias(map, "cmd", "command");
            }
            "search" | "web_search" => {
                move_alias(map, "pattern", "query");
                move_alias(map, "text", "query");
            }
            "project_map" => {
                move_alias(map, "path", "directory");
            }
            _ => {}
        }
    }

    fn validate_tool_input(&self, tool: &dyn crate::tools::Tool, input: &Value) -> Option<Value> {
        let schema = tool.input_schema();
        let compiled = match JSONSchema::options()
            .with_draft(Draft::Draft7)
            .compile(&schema)
        {
            Ok(compiled) => compiled,
            Err(err) => {
                tracing::warn!(
                    tool = tool.name(),
                    error = %err,
                    "tool schema failed to compile; skipping validation"
                );
                return None;
            }
        };

        if let Err(errors) = compiled.validate(input) {
            let details: Vec<Value> = errors
                .map(|error| {
                    json!({
                        "path": error.instance_path.to_string(),
                        "error": error.to_string()
                    })
                })
                .collect();

            return Some(json!({
                "status": "error",
                "tool": tool.name(),
                "error_type": "invalid_tool_arguments",
                "message": "Tool arguments did not match the expected schema.",
                "validation_errors": details,
                "expected_schema": schema
            }));
        }

        None
    }

    /// Validate that the tool input does not contain dangerous commands.
    fn validate_safety(&self, tool_name: &str, input: &Value) -> Result<()> {
        if tool_name == "shell" {
            if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
                let lower = command.to_lowercase();
                for blocked in BLOCKED_COMMANDS {
                    if lower.contains(blocked) {
                        bail!(
                            "🛑 BLOCKED: Dangerous command detected: '{}'. \
                             Command pattern '{}' is not allowed.",
                            command,
                            blocked
                        );
                    }
                }
            }
        }

        if tool_name == "filesystem" || tool_name == "edit" {
            if let Some(path) = input
                .get("path")
                .or_else(|| input.get("file"))
                .and_then(|p| p.as_str())
            {
                let critical_paths = [
                    "/etc", "/boot", "/dev", "/sys", "/proc", "/usr/bin", "/sbin",
                ];
                for critical in &critical_paths {
                    if path.starts_with(critical) {
                        bail!("🛑 BLOCKED: Cannot access critical system path: {}", path);
                    }
                }
            }
        }

        Ok(())
    }

    /// Validate that a shell command only uses allowed commands (sandbox mode).
    fn validate_shell_sandbox(&self, input: &Value) -> Result<()> {
        if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
            // SECURITY FIX: Block subshell injection attempts before any further parsing.
            // Attackers can bypass the allowlist by nesting arbitrary commands inside
            // $(...) or backtick `...` substitutions that get executed by the shell.
            if command.contains("$(") || command.contains('`') {
                bail!(
                    "🛑 SANDBOXED: Subshell substitution ($() or backticks) is not allowed.\n\
                     Command substitution can bypass sandbox restrictions."
                );
            }

            let separators = ['|', ';'];
            let parts: Vec<&str> = command
                .split(|c: char| separators.contains(&c))
                .chain(command.split("&&"))
                .chain(command.split("||"))
                .collect();

            for part in parts {
                let trimmed = part.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let base_cmd = trimmed
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches("./")
                    .trim_start_matches('/');

                let binary_name = base_cmd.rsplit('/').next().unwrap_or(base_cmd);

                if !binary_name.is_empty() && !ALLOWED_SHELL_COMMANDS.contains(&binary_name) {
                    bail!(
                        "🛑 SANDBOXED: Command '{}' is not in the allowed list.\n\
                         Allowed: {:?}",
                        binary_name,
                        ALLOWED_SHELL_COMMANDS
                    );
                }
            }
        }
        Ok(())
    }

    fn inject_secrets(
        &self,
        tool_name: &str,
        tool_policy: &ToolSecretPolicy,
        input: &mut Value,
        taint: &mut SecretTaintContext,
    ) -> Result<()> {
        // Check vault session is active (not locked/expired).
        let session_active = self
            .vault_session
            .read()
            .map(|s| s.is_active())
            .unwrap_or(false);
        if !session_active {
            bail!("vault session is locked or expired — unlock the vault to resolve secrets");
        }

        let vault = self
            .vault
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("vault is not configured for secret injection"))?;
        let config_policy = self.config.get_secret_tool_policy(tool_name);

        if !config_policy.allow_secrets {
            self.anomaly_detector.record_failed_attempt();
            bail!(
                "tool '{}' is not authorized for secret injection by config (allow_secrets=false)",
                tool_name
            );
        }

        if !tool_policy.allow_secrets {
            self.anomaly_detector.record_failed_attempt();
            bail!("tool '{}' does not declare allow_secrets=true", tool_name);
        }

        if config_policy.allowed_secret_keys.is_empty()
            && tool_policy.allowed_secret_keys.is_empty()
        {
            bail!(
                "tool '{}' has secret injection enabled but no allowed_secret_keys are configured",
                tool_name
            );
        }

        // Rate limiting: check per-task and per-minute limits.
        let task_id = format!("session_{}", self.tool_call_count.load(Ordering::SeqCst));
        if let Err(err) = self.rate_limiter.check_and_increment(&task_id) {
            self.anomaly_detector.record_failed_attempt();
            bail!("{}", err);
        }

        // Optional approval mode.
        if self.config.secrets.require_explicit_approval && !self.config.is_autonomous() {
            eprintln!("\n🔑 Tool '{}' requests vault secret access.", tool_name);
            eprint!("  Approve secret resolution? [y/n]: ");
            std::io::stderr().flush().ok();
            let mut response = String::new();
            std::io::stdin()
                .read_line(&mut response)
                .context("Failed to read approval input")?;
            if response.trim().to_lowercase() != "y" && response.trim().to_lowercase() != "yes" {
                bail!("Secret resolution denied by user.");
            }
        }

        let scoped_keys = self
            .config
            .get_task_secret_scope(tool_name)
            .unwrap_or_default();

        // Scope enforcement: verify each requested secret's scope is allowed.
        let keys_in_input = Self::extract_all_vault_keys_from_value(input);
        let records = vault.list_secret_records()?;
        for key in &keys_in_input {
            if let Some(record) = records.iter().find(|r| &r.key == key) {
                if !config_policy.allowed_scopes.is_empty()
                    && !config_policy
                        .allowed_scopes
                        .iter()
                        .any(|s| s == &record.scope)
                {
                    self.anomaly_detector.record_policy_violation();
                    bail!(
                        "secret '{}' (scope '{}') is not allowed for tool '{}' (allowed scopes: {:?})",
                        key,
                        record.scope,
                        tool_name,
                        config_policy.allowed_scopes
                    );
                }
            }
        }

        self.replace_vault_refs(
            input,
            vault,
            &config_policy,
            tool_policy,
            &scoped_keys,
            taint,
        )?;

        // Emit telemetry events for each resolved secret.
        for key in &keys_in_input {
            let event = SecretAccessEvent::new(
                &task_id,
                tool_name,
                key,
                "allowed",
                &self.executor_instance_id,
                "config_policy",
            );
            event.emit();
        }

        // Update anomaly detector.
        self.anomaly_detector.record_resolution(&task_id, tool_name);

        Ok(())
    }

    fn contains_vault_refs(&self, value: &Value) -> bool {
        match value {
            Value::String(s) => s.contains("{{vault:") && s.contains("}}"),
            Value::Object(map) => map.values().any(|v| self.contains_vault_refs(v)),
            Value::Array(arr) => arr.iter().any(|v| self.contains_vault_refs(v)),
            _ => false,
        }
    }

    fn replace_vault_refs(
        &self,
        value: &mut Value,
        vault: &SecretVault,
        config_policy: &SecretToolPolicy,
        tool_policy: &ToolSecretPolicy,
        scoped_keys: &[String],
        taint: &mut SecretTaintContext,
    ) -> Result<()> {
        match value {
            Value::String(s) => {
                let keys = Self::extract_vault_placeholder_keys(s);
                if keys.is_empty() {
                    return Ok(());
                }
                let records = vault.list_secret_records()?;
                let mut replaced = s.clone();

                for key in keys {
                    if !config_policy.allowed_secret_keys.is_empty()
                        && !config_policy
                            .allowed_secret_keys
                            .iter()
                            .any(|allowed| allowed == &key)
                    {
                        bail!(
                            "secret key '{}' is not allowed for this tool by policy",
                            key
                        );
                    }

                    if !tool_policy.allowed_secret_keys.is_empty()
                        && !tool_policy
                            .allowed_secret_keys
                            .iter()
                            .any(|allowed| allowed == &key)
                    {
                        bail!(
                            "secret key '{}' is not declared by the tool secret policy",
                            key
                        );
                    }

                    if !scoped_keys.is_empty() && !scoped_keys.iter().any(|allowed| allowed == &key)
                    {
                        bail!("secret key '{}' is outside the current task scope", key);
                    }

                    let record = records
                        .iter()
                        .find(|record| record.key == key)
                        .ok_or_else(|| anyhow::anyhow!("vault key '{}' not found", key))?;

                    let placeholder = format!("{{{{vault:{}}}}}", key);
                    replaced = replaced.replace(&placeholder, &record.value);
                    taint.add(record);
                }
                *value = Value::String(replaced);
                Ok(())
            }
            Value::Object(map) => {
                for (_, nested) in map.iter_mut() {
                    self.replace_vault_refs(
                        nested,
                        vault,
                        config_policy,
                        tool_policy,
                        scoped_keys,
                        taint,
                    )?;
                }
                Ok(())
            }
            Value::Array(arr) => {
                for nested in arr.iter_mut() {
                    self.replace_vault_refs(
                        nested,
                        vault,
                        config_policy,
                        tool_policy,
                        scoped_keys,
                        taint,
                    )?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn extract_vault_placeholder_keys(input: &str) -> Vec<String> {
        let mut keys = Vec::new();
        let mut cursor = input;

        while let Some(start) = cursor.find("{{vault:") {
            let rest = &cursor[start + "{{vault:".len()..];
            let Some(end) = rest.find("}}") else {
                break;
            };
            let key = rest[..end].trim();
            if !key.is_empty() {
                keys.push(key.to_string());
            }
            cursor = &rest[end + 2..];
        }

        keys
    }

    fn redact_secrets(&self, value: &mut Value, taint: &SecretTaintContext) {
        let mut records: Vec<SecretExposure> = taint.exposures.clone();
        if let Some(vault) = &self.vault {
            if let Ok(secret_records) = vault.list_secret_records() {
                for record in secret_records {
                    records.push(SecretExposure {
                        key: record.key,
                        value: record.value,
                        fingerprint: record.fingerprint,
                        token_fingerprints: record.token_fingerprints,
                    });
                }
            }
        }

        for record in records {
            self.redact_value(value, &record);
        }
    }

    /// Normalize output text to defeat formatting bypass attacks (e.g. spaced
    /// characters), then run the full redaction engine.
    fn normalize_and_redact_secrets(&self, value: &mut Value, taint: &SecretTaintContext) {
        // First pass: standard redaction on original output.
        self.redact_secrets(value, taint);

        // Second pass: normalize text and check for hidden secret fragments.
        self.redact_normalized(value, taint);
    }

    /// Strip whitespace/tabs/newlines from string values and check if the
    /// normalized form contains any secret values or fingerprints.
    fn redact_normalized(&self, value: &mut Value, taint: &SecretTaintContext) {
        if let Value::String(s) = value {
            let normalized: String = s.chars().filter(|c| !c.is_whitespace()).collect();

            let mut records: Vec<SecretExposure> = taint.exposures.clone();
            if let Some(vault) = &self.vault {
                if let Ok(secret_records) = vault.list_secret_records() {
                    for record in secret_records {
                        records.push(SecretExposure {
                            key: record.key,
                            value: record.value,
                            fingerprint: record.fingerprint,
                            token_fingerprints: record.token_fingerprints,
                        });
                    }
                }
            }

            for record in &records {
                if !record.value.is_empty() {
                    let normalized_secret: String = record
                        .value
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect();
                    if normalized.contains(&normalized_secret) {
                        // The original string contains a spaced-out secret — redact the whole thing.
                        *s = REDACTED_VALUE.to_string();
                        return;
                    }
                }
            }
        } else if let Value::Object(map) = value {
            for nested in map.values_mut() {
                self.redact_normalized(nested, taint);
            }
        } else if let Value::Array(arr) = value {
            for nested in arr.iter_mut() {
                self.redact_normalized(nested, taint);
            }
        }
    }

    /// Extract all vault placeholder keys from a JSON value tree.
    fn extract_all_vault_keys_from_value(value: &Value) -> Vec<String> {
        let mut keys = Vec::new();
        match value {
            Value::String(s) => {
                keys.extend(Self::extract_vault_placeholder_keys(s));
            }
            Value::Object(map) => {
                for v in map.values() {
                    keys.extend(Self::extract_all_vault_keys_from_value(v));
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    keys.extend(Self::extract_all_vault_keys_from_value(v));
                }
            }
            _ => {}
        }
        keys
    }

    fn redact_with_taint(&self, value: &mut Value, taint: &SecretTaintContext) {
        for record in &taint.exposures {
            self.redact_value(value, record);
        }
    }

    fn redact_value(&self, value: &mut Value, record: &SecretExposure) {
        match value {
            Value::String(s) => {
                if s.contains(&record.value) {
                    *s = s.replace(&record.value, REDACTED_VALUE);
                }
                if s.contains(&record.fingerprint) {
                    *s = s.replace(&record.fingerprint, REDACTED_VALUE);
                }

                let token_fingerprints: HashSet<&str> = record
                    .token_fingerprints
                    .iter()
                    .map(|fp| fp.as_str())
                    .collect();
                let mut replaced = s.clone();
                // Use HMAC-based fingerprint comparison when vault is available.
                let fp_key = self.vault.as_ref().map(|v| *v.fingerprint_key());
                for token in Self::extract_alnum_tokens(s) {
                    if token.len() < 4 {
                        continue;
                    }
                    let matches = if let Some(ref fk) = fp_key {
                        let hmac_fp = hmac_fingerprint(token.as_bytes(), fk);
                        hmac_fp == record.fingerprint
                            || token_fingerprints.contains(hmac_fp.as_str())
                    } else {
                        let fp = Self::sha256_hex(token.as_bytes());
                        fp == record.fingerprint || token_fingerprints.contains(fp.as_str())
                    };
                    if matches {
                        replaced = replaced.replace(&token, REDACTED_VALUE);
                    }
                }
                *s = replaced;
            }
            Value::Object(map) => {
                for nested in map.values_mut() {
                    self.redact_value(nested, record);
                }
            }
            Value::Array(arr) => {
                for nested in arr.iter_mut() {
                    self.redact_value(nested, record);
                }
            }
            _ => {}
        }
    }

    fn redact_text_with_taint(&self, text: &str, taint: &SecretTaintContext) -> String {
        let mut redacted = text.to_string();
        for record in &taint.exposures {
            if !record.value.is_empty() {
                redacted = redacted.replace(&record.value, REDACTED_VALUE);
            }
            if !record.fingerprint.is_empty() {
                redacted = redacted.replace(&record.fingerprint, REDACTED_VALUE);
            }
        }
        redacted
    }

    fn scrub_tainted_value(&self, value: &mut Value, taint: &SecretTaintContext) {
        if !taint.contains_secret_input {
            return;
        }

        match value {
            Value::String(s) => {
                if taint
                    .exposures
                    .iter()
                    .any(|exposure| !exposure.value.is_empty() && s.contains(&exposure.value))
                {
                    Self::wipe_string(s);
                }
            }
            Value::Object(map) => {
                for nested in map.values_mut() {
                    self.scrub_tainted_value(nested, taint);
                }
            }
            Value::Array(arr) => {
                for nested in arr.iter_mut() {
                    self.scrub_tainted_value(nested, taint);
                }
            }
            _ => {}
        }
    }

    fn wipe_string(target: &mut String) {
        if target.is_empty() {
            return;
        }
        // Overwrite the string bytes with random data, then zeroize and clear.
        let mut random = vec![0u8; target.len()];
        OsRng.fill_bytes(&mut random);
        for byte in &mut random {
            // keep printable ASCII so we can safely convert to UTF-8 string
            *byte = 33 + (*byte % 94);
        }
        if let Ok(random_text) = String::from_utf8(random) {
            *target = random_text;
        }
        // Zeroize the underlying bytes for defense-in-depth.
        unsafe {
            target.as_bytes_mut().zeroize();
        }
        target.clear();
    }

    fn extract_alnum_tokens(text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        for ch in text.chars() {
            if ch.is_ascii_alphanumeric() {
                current.push(ch);
            } else if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        tokens
    }

    fn sha256_hex(input: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input);
        format!("{:x}", hasher.finalize())
    }

    pub fn llm_response_contains_secret(&self, text: &str) -> bool {
        let Some(vault) = &self.vault else {
            return false;
        };
        let Ok(records) = vault.list_secret_records() else {
            return false;
        };

        for record in records {
            if (!record.value.is_empty() && text.contains(&record.value))
                || text.contains(&record.fingerprint)
            {
                return true;
            }
            for token in Self::extract_alnum_tokens(text) {
                if token.len() < 4 {
                    continue;
                }
                let token_hash = Self::sha256_hex(token.as_bytes());
                if token_hash == record.fingerprint
                    || record.token_fingerprints.iter().any(|fp| fp == &token_hash)
                {
                    return true;
                }
            }
        }

        false
    }

    pub fn redact_output_for_security(&self, value: &mut Value) {
        self.redact_secrets(value, &SecretTaintContext::default());
    }

    fn error_response(
        tool_name: &str,
        error_type: &str,
        message: impl Into<String>,
        suggestion: &str,
    ) -> Value {
        json!({
            "status": "error",
            "tool": tool_name,
            "error_type": error_type,
            "message": message.into(),
            "suggestion": suggestion
        })
    }

    fn normalize_tool_output(tool_name: &str, value: Value) -> Value {
        match value {
            Value::Object(mut map) => {
                let existing_error = map
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let status_text = map
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                match status_text {
                    Some(_) => {}
                    None => {
                        if let Some(err) = existing_error {
                            return Self::error_response(
                                tool_name,
                                "tool_error",
                                format!("tool reported error: {}", err),
                                "Adjust tool arguments and retry.",
                            );
                        }

                        // Preserve non-string status values (e.g. numeric HTTP codes) without breaking output shape.
                        if let Some(raw_status) = map.get("status").cloned() {
                            map.insert("tool_status".to_string(), raw_status);
                        }
                        map.insert("status".to_string(), Value::String("ok".to_string()));
                    }
                }

                if map
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(|s| s.eq_ignore_ascii_case("error"))
                    .unwrap_or(false)
                    && map.get("message").is_none()
                {
                    if let Some(err) = map.get("error").and_then(|v| v.as_str()) {
                        map.insert("message".to_string(), Value::String(err.to_string()));
                    }
                }

                let has_message = map.get("message").is_some();
                if !map.contains_key("data") && !has_message {
                    let mut data = map.clone();
                    data.remove("status");
                    map.insert("data".to_string(), Value::Object(data));
                }

                Value::Object(map)
            }
            other => json!({
                "status": "ok",
                "tool": tool_name,
                "data": {
                    "value": other,
                }
            }),
        }
    }

    fn loggable_json(value: &Value) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
    }

    fn value_is_error(value: &Value) -> bool {
        value
            .get("status")
            .and_then(|v| v.as_str())
            .map(|status| status.eq_ignore_ascii_case("error"))
            .unwrap_or(false)
            || value.get("error").is_some()
    }

    fn extract_error_message(value: &Value) -> Option<String> {
        value
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
    }
}
