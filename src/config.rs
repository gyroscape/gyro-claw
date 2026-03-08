//! # Configuration System
//!
//! Loads tool permission policies and agent limits from `~/.gyro-claw/config.toml`.
//!
//! Supports three permission modes per tool:
//! - `allow` → runs automatically
//! - `ask`   → prompts user for confirmation
//! - `deny`  → blocks execution entirely
//!
//! If no config file exists, creates a default one in safe mode.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Tool permission policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ToolPermission {
    Allow,
    #[default]
    Ask,
    Deny,
}


/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Execution mode: "safe" (default) or "autonomous"
    #[serde(default = "default_mode")]
    pub mode: String,

    /// Maximum LLM iterations per run
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,

    /// Maximum total tool calls per session
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: usize,

    /// Maximum recoverable retries per step before replanning/aborting
    #[serde(default = "default_max_retries")]
    pub max_retries: usize,

    /// Backoff between retries for tool failures
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: u64,

    /// Per-tool permission policies
    #[serde(default)]
    pub safety: SafetyConfig,

    #[serde(default)]
    pub execution: ExecutionConfig,

    #[serde(default)]
    pub memory: MemoryConfig,

    #[serde(default)]
    pub sandbox: SandboxConfig,

    #[serde(default)]
    pub computer_control: ComputerControlConfig,

    #[serde(default)]
    pub browser: BrowserConfig,

    /// Secret-injection policy engine (zero-trust secret scope controls).
    #[serde(default)]
    pub secrets: SecretPolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_seconds: u64,
    #[serde(default = "default_max_task_runtime")]
    pub max_task_runtime_seconds: u64,
    #[serde(default = "default_max_shell_runtime")]
    pub max_shell_runtime_seconds: u64,
    #[serde(default = "default_max_browser_requests")]
    pub max_browser_requests: usize,
    #[serde(default = "default_max_tool_memory")]
    pub max_tool_memory_mb: usize,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            tool_timeout_seconds: default_tool_timeout(),
            max_task_runtime_seconds: default_max_task_runtime(),
            max_shell_runtime_seconds: default_max_shell_runtime(),
            max_browser_requests: default_max_browser_requests(),
            max_tool_memory_mb: default_max_tool_memory(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_max_project_facts")]
    pub max_project_facts: usize,
    #[serde(default = "default_max_logs")]
    pub max_logs: usize,
    #[serde(default = "default_max_events")]
    pub max_events: usize,
    #[serde(default = "default_max_screenshots")]
    pub max_screenshots: usize,
    #[serde(default = "default_max_semantic_chunks")]
    pub max_semantic_chunks: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_project_facts: default_max_project_facts(),
            max_logs: default_max_logs(),
            max_events: default_max_events(),
            max_screenshots: default_max_screenshots(),
            max_semantic_chunks: default_max_semantic_chunks(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    #[serde(default = "default_workspace")]
    pub workspace: String,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            workspace: default_workspace(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputerControlConfig {
    #[serde(default = "default_computer_control_enabled")]
    pub enabled: bool,
    #[serde(default = "default_allowed_apps")]
    pub allowed_apps: Vec<String>,
    #[serde(default = "default_block_system_paths")]
    pub block_system_paths: bool,
    #[serde(default = "default_max_actions_per_cycle")]
    pub max_actions_per_cycle: usize,
    #[serde(default = "default_ui_detection_enabled")]
    pub ui_detection_enabled: bool,
    #[serde(default = "default_screen_change_threshold")]
    pub screen_change_threshold: f32,
    #[serde(default = "default_max_ui_retries")]
    pub max_ui_retries: usize,
}

impl Default for ComputerControlConfig {
    fn default() -> Self {
        Self {
            enabled: default_computer_control_enabled(),
            allowed_apps: default_allowed_apps(),
            block_system_paths: default_block_system_paths(),
            max_actions_per_cycle: default_max_actions_per_cycle(),
            ui_detection_enabled: default_ui_detection_enabled(),
            screen_change_threshold: default_screen_change_threshold(),
            max_ui_retries: default_max_ui_retries(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    #[serde(default = "default_wait_timeout")]
    pub default_wait_timeout: u64,
    #[serde(default = "default_max_navigation_timeout")]
    pub max_navigation_timeout: u64,
}

/// Secret policy for a single tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct SecretToolPolicy {
    #[serde(default)]
    pub allow_secrets: bool,
    #[serde(default)]
    pub allowed_secret_keys: Vec<String>,
    /// Allowed secret scopes for this tool (e.g. ["github", "openai"]).
    /// Empty means all scopes are allowed.
    #[serde(default)]
    pub allowed_scopes: Vec<String>,
}


/// Global secret-policy configuration.
///
/// `tool_permissions` controls whether each tool may resolve vault placeholders.
/// `task_permissions` provides a stricter per-tool scope of key names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretPolicyConfig {
    #[serde(default)]
    pub tool_permissions: HashMap<String, SecretToolPolicy>,
    #[serde(default)]
    pub task_permissions: HashMap<String, Vec<String>>,
    /// Maximum secret resolutions allowed per task (rate limiting).
    #[serde(default = "default_max_secret_resolutions_per_task")]
    pub max_secret_resolutions_per_task: usize,
    /// Maximum secret resolutions allowed per minute globally.
    #[serde(default = "default_max_secret_resolutions_per_minute")]
    pub max_secret_resolutions_per_minute: usize,
    /// Duration in seconds the vault remains unlocked after authentication.
    #[serde(default = "default_vault_session_duration")]
    pub vault_session_duration_seconds: u64,
    /// When true, executor prompts for confirmation before resolving secrets.
    #[serde(default)]
    pub require_explicit_approval: bool,
    /// Anomaly alert threshold (alerts fire when any metric exceeds this).
    #[serde(default = "default_anomaly_alert_threshold")]
    pub anomaly_alert_threshold: usize,
}

impl Default for SecretPolicyConfig {
    fn default() -> Self {
        let mut tool_permissions = HashMap::new();
        tool_permissions.insert(
            "http".to_string(),
            SecretToolPolicy {
                allow_secrets: true,
                allowed_secret_keys: vec!["api_key".to_string(), "auth_token".to_string()],
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "shell".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "filesystem".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "edit".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "git".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "search".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "web_search".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );
        tool_permissions.insert(
            "web_fetch".to_string(),
            SecretToolPolicy {
                allow_secrets: false,
                allowed_secret_keys: Vec::new(),
                allowed_scopes: Vec::new(),
            },
        );

        let mut task_permissions = HashMap::new();
        task_permissions.insert(
            "http".to_string(),
            vec!["api_key".to_string(), "auth_token".to_string()],
        );

        Self {
            tool_permissions,
            task_permissions,
            max_secret_resolutions_per_task: default_max_secret_resolutions_per_task(),
            max_secret_resolutions_per_minute: default_max_secret_resolutions_per_minute(),
            vault_session_duration_seconds: default_vault_session_duration(),
            require_explicit_approval: false,
            anomaly_alert_threshold: default_anomaly_alert_threshold(),
        }
    }
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            default_wait_timeout: default_wait_timeout(),
            max_navigation_timeout: default_max_navigation_timeout(),
        }
    }
}

/// Per-tool safety permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    #[serde(default = "default_allow")]
    pub filesystem: ToolPermission,

    #[serde(default = "default_ask")]
    pub shell: ToolPermission,

    #[serde(default = "default_ask")]
    pub http: ToolPermission,

    #[serde(default = "default_ask")]
    pub edit: ToolPermission,

    #[serde(default = "default_ask")]
    pub git: ToolPermission,

    #[serde(default = "default_allow")]
    pub search: ToolPermission,

    #[serde(default = "default_allow")]
    pub project_map: ToolPermission,

    #[serde(default = "default_ask")]
    pub web_search: ToolPermission,

    #[serde(default = "default_ask")]
    pub test_runner: ToolPermission,

    /// Catch-all for tools not explicitly listed
    #[serde(flatten)]
    pub custom: HashMap<String, ToolPermission>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            filesystem: ToolPermission::Allow,
            shell: ToolPermission::Ask,
            http: ToolPermission::Ask,
            edit: ToolPermission::Ask,
            git: ToolPermission::Ask,
            search: ToolPermission::Allow,
            project_map: ToolPermission::Allow,
            web_search: ToolPermission::Ask,
            test_runner: ToolPermission::Ask,
            custom: HashMap::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: "safe".to_string(),
            max_iterations: default_max_iterations(),
            max_tool_calls: default_max_tool_calls(),
            max_retries: default_max_retries(),
            retry_backoff_ms: default_retry_backoff_ms(),
            safety: SafetyConfig::default(),
            execution: ExecutionConfig::default(),
            memory: MemoryConfig::default(),
            sandbox: SandboxConfig::default(),
            computer_control: ComputerControlConfig::default(),
            browser: BrowserConfig::default(),
            secrets: SecretPolicyConfig::default(),
        }
    }
}

impl Config {
    /// Load config from `~/.gyro-claw/config.toml`.
    /// If the file doesn't exist, creates a default one and returns it.
    pub fn load() -> Result<Self> {
        let config_path = config_path();

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config: {:?}", config_path))?;
            let config: Config = toml::from_str(&content).context("Failed to parse config.toml")?;
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    /// Save current config to `~/.gyro-claw/config.toml`.
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(&path, content).context("Failed to write config.toml")?;
        Ok(())
    }

    /// Get the permission policy for a tool by name.
    pub fn get_tool_permission(&self, tool_name: &str) -> ToolPermission {
        match tool_name {
            "filesystem" => self.safety.filesystem.clone(),
            "shell" => self.safety.shell.clone(),
            "http" => self.safety.http.clone(),
            "edit" => self.safety.edit.clone(),
            "git" => self.safety.git.clone(),
            "search" => self.safety.search.clone(),
            "project_map" => self.safety.project_map.clone(),
            "web_search" => self.safety.web_search.clone(),
            "test_runner" => self.safety.test_runner.clone(),
            other => self
                .safety
                .custom
                .get(other)
                .cloned()
                .unwrap_or(ToolPermission::Ask),
        }
    }

    /// Check if we're running in autonomous mode.
    pub fn is_autonomous(&self) -> bool {
        self.mode == "autonomous"
    }

    pub fn get_secret_tool_policy(&self, tool_name: &str) -> SecretToolPolicy {
        self.secrets
            .tool_permissions
            .get(tool_name)
            .cloned()
            .or_else(|| self.secrets.tool_permissions.get("*").cloned())
            .unwrap_or_default()
    }

    pub fn get_task_secret_scope(&self, tool_name: &str) -> Option<Vec<String>> {
        self.secrets
            .task_permissions
            .get(tool_name)
            .cloned()
            .or_else(|| self.secrets.task_permissions.get("*").cloned())
    }
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".gyro-claw").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

fn default_mode() -> String {
    "safe".to_string()
}
fn default_max_iterations() -> usize {
    20
}
fn default_max_tool_calls() -> usize {
    30
}
fn default_max_retries() -> usize {
    3
}
fn default_retry_backoff_ms() -> u64 {
    500
}
fn default_allow() -> ToolPermission {
    ToolPermission::Allow
}
fn default_ask() -> ToolPermission {
    ToolPermission::Ask
}
fn default_tool_timeout() -> u64 {
    45
}
fn default_max_task_runtime() -> u64 {
    600
}
fn default_max_shell_runtime() -> u64 {
    60
}
fn default_max_browser_requests() -> usize {
    20
}
fn default_max_tool_memory() -> usize {
    512
}
fn default_max_project_facts() -> usize {
    1000
}
fn default_max_logs() -> usize {
    5000
}
fn default_max_events() -> usize {
    1000
}
fn default_max_screenshots() -> usize {
    200
}
fn default_max_semantic_chunks() -> usize {
    10000
}
fn default_workspace() -> String {
    "./workspace".to_string()
}
fn default_computer_control_enabled() -> bool {
    true
}
fn default_allowed_apps() -> Vec<String> {
    vec![
        "Google Chrome".to_string(),
        "Safari".to_string(),
        "Terminal".to_string(),
        "VSCode".to_string(),
        "Finder".to_string(),
    ]
}
fn default_block_system_paths() -> bool {
    true
}
fn default_max_actions_per_cycle() -> usize {
    10
}
fn default_ui_detection_enabled() -> bool {
    true
}
fn default_screen_change_threshold() -> f32 {
    2.0
}
fn default_max_ui_retries() -> usize {
    3
}

fn default_wait_timeout() -> u64 {
    10
}

fn default_max_navigation_timeout() -> u64 {
    20
}

fn default_max_secret_resolutions_per_task() -> usize {
    5
}
fn default_max_secret_resolutions_per_minute() -> usize {
    20
}
fn default_vault_session_duration() -> u64 {
    600
}
fn default_anomaly_alert_threshold() -> usize {
    10
}
