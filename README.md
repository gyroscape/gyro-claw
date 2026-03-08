# 🦀 Gyro-Claw

**A lightweight, privacy-first AI automation agent built in Rust.**

Gyro-Claw runs locally, lets an AI model plan tasks and call tools, but **never exposes user passwords, tokens, or secrets to the AI model**.

> [!NOTE]
> Gyro-Claw is early in development. We are constantly working to improve the experience and add new features.

---

## ✨ Features

- **🔒 Privacy-First** — Secrets are AES-256-GCM encrypted and never sent to the LLM
- **🛡️ Secure Execution** — Dangerous commands are blocked, inputs are validated
- **🔧 Modular Tools** — 9 powerful tools built-in (filesystem, shell, http, git, search, edit, project_map, web_search, test_runner)
- **⚙️ Configurable Safety** — TOML-based permission policies (Safe vs Autonomous modes)
- **🧠 Memory** — SQLite-backed conversation and task history
- **🤖 Swappable LLM** — Gyroscape (default), OpenRouter, or any OpenAI-compatible API
- **📋 Multi-Step Tasks** — Plan and execute complex workflows with task state tracking
- **🛠️ Auto-Fix Loop** — Autonomous test-driven development: Agent runs tests, edits code, and repeats until all pass
- **🖥️ Computer Control** — Control your desktop via mouse, keyboard, and vision-based UI detection
- **🌐 Browser Automation** — High-reliability web automation with multi-selector fallbacks and Playwright integration
- **🩺 UI State Machine** — Strict Observe → Decide → Act → Verify loop to prevent infinite vision loops
- **🐳 Docker Ready** — Development and production Docker support

---

## 📁 Project Structure

```
├── playwright-server/       # External Playwright Node.js service
│   ├── server.js            # Express API for browser actions
│   └── package.json
├── src/
│   ├── main.rs              # CLI entrypoint (Clap + Tokio)
│   ├── agent/
│   │   ├── mod.rs            # Agent module
│   │   ├── planner.rs        # Agent loop (Observe → Decide → Act → Verify)
│   │   ├── executor.rs       # Secure execution layer
│   │   ├── memory.rs         # SQLite conversation & task history
│   │   └── task.rs           # Multi-step task state
│   ├── tools/
│   │   ├── mod.rs            # Tool trait & registry
│   │   ├── browser.rs        # Selenium-like browser control with fallbacks
│   │   ├── playwright.rs     # Integration with Node.js Playwright server
│   │   ├── computer/         # OS-level control (Mouse, Keyboard, Screenshot)
│   │   ├── shell.rs          # Shell command execution
│   │   ├── filesystem.rs     # File read/write/list
│   │   └── http.rs           # HTTP API requests
│   ├── vault/
│   │   ├── mod.rs            # Vault module
│   │   └── secrets.rs        # AES-256-GCM encrypted secret storage
│   ├── llm/
│   │   ├── mod.rs            # LLM provider trait
│   │   └── client.rs         # OpenRouter / Groq client
│   └── api/
│       ├── mod.rs            # API module
│       └── server.rs         # Axum web API
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
└── README.md
```

---

## 🚀 Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (1.70+)
- An LLM API key (Gyroscape, OpenRouter, or any OpenAI-compatible API)

### Build

```bash
cargo build --release
```

### Set API Key Securely

Gyro-Claw uses an AES-256-GCM encrypted local vault so your API keys are never exposed in plain text.

1. Ensure your master password is set in `.env` (or exported via bash):
```bash
echo "GYRO_CLAW_VAULT_PASSWORD=your_secure_password" > .env
```

2. Store your LLM provider key securely:
```bash
# Gyroscape (Default)
gyro-claw vault set GYROSCAPE_API_KEY

# OpenRouter (Alternative)
gyro-claw vault set OPENROUTER_API_KEY
```
*(You will be securely prompted to enter the secret value without it echoing to the terminal.)*

### Run a Command

```bash
gyro-claw run "list all files in the current directory"
```

### Interactive Chat

```bash
gyro-claw chat
```

### 🤖 Integration Bots (Telegram & Slack)

You can run Gyro-Claw as a background listener on Telegram or Slack. The command automatically spins up the listener **and** a background worker process simultaneously to execute the commands.

**To run the Telegram Bot:**
```bash
# Recommended to run in autonomous mode for seamless execution
gyro-claw config mode autonomous

# Run the unified bot and background worker
cargo run -- bot telegram
```

It will interactively ask you for your bot token (`TELOXIDE_TOKEN`) and user ID (`TELEGRAM_ALLOWED_USER_ID`) if they are missing from your vault, and securely save them. Alternatively, you can pre-configure them:

```bash
gyro-claw vault set TELOXIDE_TOKEN
gyro-claw vault set TELEGRAM_ALLOWED_USER_ID
```

**Supported Commands in Telegram:**
- `/start` or `/help` - Show instructions
- `/run <goal>` - Queue a background task for the worker to execute 
- `/status` - Check if the listener is online
- `/tasks` - View recently queued tasks
- `/stop <id>` - Cancel a task

---

### 🛠️ Auto-Fix Loop (Autonomous TDD)

Have the agent automatically run tests, find failures, read the code, apply fixes, and re-test until everything passes:

```bash
# Fix all tests in the project
gyro-claw autofix

# Fix a specific test
gyro-claw autofix --test-name my_failing_test
```
*(Note: For maximum effectiveness, run `gyro-claw config mode autonomous` first so you don't have to manually approve every file edit and test run).*

### Start Web API Server

```bash
gyro-claw serve --port 3000
```

The server binds to `127.0.0.1` (localhost only) by default for security. Configure via environment variables:

```bash
# Allow external access (use with caution)
export GYRO_CLAW_HOST="0.0.0.0"

# Set allowed CORS origin
export GYRO_CLAW_CORS_ORIGIN="http://localhost:3000"
```

---

## 🌐 Advanced Browser Automation

Gyro-Claw prioritizes stability by combining vision-based detection with API-driven automation.

### 🎭 Playwright Integration
For high-reliability website interaction, Gyro-Claw uses an external Playwright service.

**1. Setup Playwright Server:**
```bash
cd playwright-server
npm install
npx playwright install chromium
```

**2. Start the Server:**
```bash
node server.js
```
The server runs on `http://127.0.0.1:4000`. Set `GYRO_CLAW_PLAYWRIGHT_ENDPOINT` in your environment to override.

### 👓 Vision-First Workflow
If Playwright is unavailable, the agent falls back to a deterministic Vision workflow:
1. `screenshot` → Capture state
2. `ui_detector` → Find element coordinates
3. `mouse.click` → Interact at pixel-perfect positions
4. `screenshot` + `screen_diff` → Verify the UI changed

---

## 🛠️ Built-in Tools

| Tool | Capability |
|------|------------|
| `playwright` | High-level web automation (open_url, search, click, type) via Node.js service |
| `browser` | DOM-based browser control with multi-selector fallback mechanisms |
| `ui_detector`| Vision-based UI element detection (bounding boxes) |
| `screenshot` | Capture display state for vision-based planning |
| `mouse` | Control cursor: click, move, drag, scroll |
| `keyboard` | Type text and press keys (Enter, Tab, etc.) |
| `screen_diff`| Detect if the screen changed after an action |
| `app_state` | Get active window bounds and application info |
| `filesystem` | Read, list directories |
| `edit` | Create, append, replace, insert, and delete lines in files |
| `shell` | Execute local commands (sandboxed by default) |
| `http` | Make web requests (supports injected vault secrets) |
| `search` | Find keywords across codebase files |
| `project_map` | Generate a directory tree representation |
| `git` | Version control: status, diff, log, commit, add, branch |
| `web_search` | Search the internet via DuckDuckGo |
| `test_runner`| Run `cargo test`, `build`, `check`, `clippy` |

---

## ⚙️ Configuration & Safety

Gyro-Claw uses a permission-based safety system. Configuration is stored at `~/.gyro-claw/config.toml`.

### Modes
- **Safe Mode (Default)**: Dangerous/mutating tools require your explicit confirmation `[y/n]` before execution.
- **Autonomous Mode**: All tools run automatically without prompting.

To switch modes:
```bash
gyro-claw config mode autonomous
gyro-claw config mode safe
```

To view or reset config:
```bash
gyro-claw config show
gyro-claw config reset
```

### Manual Policy Customization
You can edit `~/.gyro-claw/config.toml` to set granular permissions per tool:
- `allow` → Runs automatically
- `ask` → Prompts you for confirmation
- `deny` → Blocked entirely

Example `config.toml`:
```toml
mode = "safe"
max_iterations = 15
max_tool_calls = 20

[safety]
filesystem = "allow"
search = "allow"
project_map = "allow"
shell = "ask"
edit = "ask"
git = "ask"
http = "ask"
web_search = "ask"
test_runner = "allow"
```

---

## 🔐 Secret Vault

Store secrets securely — they are encrypted with AES-256-GCM and never exposed to the AI model.

```bash
# Store a secret (reads value from stdin for security)
gyro-claw vault set MY_API_KEY
# Then type your secret and press Enter
# Or pipe it: echo "sk-secret-value" | gyro-claw vault set MY_API_KEY

# List stored keys (values are NOT shown)
gyro-claw vault list

# Retrieve a secret (masked output — only first/last chars shown)
gyro-claw vault get MY_API_KEY

# Remove a secret
gyro-claw vault remove MY_API_KEY
```

The AI model can reference secrets using `{{vault:KEY_NAME}}` in tool arguments. The executor injects the actual values at runtime — the LLM never sees them.

---

## 🐳 Docker

### Production

```bash
docker-compose up gyro-claw
```

### Development

```bash
docker-compose --profile dev up gyro-claw-dev
```

---

## 🛡️ Security Architecture

This project is designed with a **zero-trust secret handling model** to ensure that sensitive data (API keys, tokens, credentials) is never exposed to the language model.

The language model only interacts with **vault placeholders**, while secret resolution occurs exclusively inside the executor layer.

Example placeholder:

```
{{vault:api_key}}
```

The model never receives the resolved secret value.

---

### Core Security Guarantees

#### LLM Isolation

Secrets are never inserted into the model prompt or context window.

The model only sees placeholders, while the executor resolves secrets at runtime during tool execution.

Architecture flow:

```
User Prompt
      ↓
LLM Planner
      ↓
Executor
      ↓
Vault Secret Resolution
      ↓
Tool Execution
```

This prevents prompt injection attacks from extracting secrets.

#### Tool-Level Secret Policies

Each tool declares whether it can receive secrets and which secret keys are allowed.

Example policy:

```
http tool
  allow_secrets: true
  allowed_secret_keys: ["api_key"]
```

Unauthorized tools such as shell commands cannot access secrets.

If a tool attempts to resolve a secret without permission, execution is blocked.

#### Redaction Engine

All tool outputs pass through a redaction layer before reaching:

* the language model
* logs
* the UI

If a secret appears in output, it is automatically replaced with:

```
[REDACTED_SECRET]
```

This protects against accidental leaks from external services.

#### Vault Encryption

Secrets stored in the vault are protected using modern cryptography:

* AES-256-GCM encryption
* Argon2 key derivation
* HKDF-based key separation
* HMAC fingerprinting for leak detection

This ensures secrets remain secure both at rest and in memory.

#### Secure Logging

Logs are protected using a redaction system and hash-chained integrity checks.

Secrets are never written to:

* debug logs
* tool logs
* telemetry

If a secret appears in tool output, it is sanitized before logging.

#### Rate Limiting and Anomaly Detection

The executor monitors secret usage patterns to prevent abuse.

Protections include:

* secret resolution rate limits
* per-task usage monitoring
* anomaly detection for suspicious behavior

If abnormal secret usage is detected, the system emits a security alert.

---

### Security Philosophy

The system follows a simple rule:

**The LLM should never have access to secrets.**

All secret handling occurs in trusted executor code outside of the model context.

This design prevents:

* prompt injection attacks
* secret exfiltration through tool responses
* accidental leaks through logs

---

### Security Audit

A built-in command validates the vault security configuration:

```bash
gyro-claw security-audit
```

This runs automated checks including:

* tool policy enforcement
* secret redaction verification
* encryption integrity checks
* rate limiting validation

Example output:

```
Vault Security Audit
--------------------
Policy enforcement: PASS
LLM isolation: PASS
Redaction engine: PASS
Encryption integrity: PASS
Log protection: PASS
```

---

### Summary

The system uses a layered security architecture to ensure secrets remain protected at all times:

* LLM never sees raw secrets
* executor-only secret resolution
* tool-level permission policies
* automatic redaction
* encrypted vault storage
* secure audit logging

This approach allows AI agents to safely use credentials without exposing them to the model.

### Running Security Tests

Verify secret isolation with the built-in test suite:

```bash
cargo test --test secret_isolation_tests
```

This runs 12 tests validating that secrets never reach the LLM through any code path.

---

## 🔧 Adding Custom Tools

Implement the `Tool` trait in `src/tools/`:

```rust
use async_trait::async_trait;
use anyhow::Result;
use serde_json::Value;
use crate::tools::Tool;

pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Description for the AI model" }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "param": { "type": "string", "description": "A parameter" }
            },
            "required": ["param"]
        })
    }
    async fn execute(&self, input: Value) -> Result<Value> {
        // Your tool logic here
        Ok(serde_json::json!({"result": "done"}))
    }
}
```

Then register it in `main.rs`:

```rust
tool_registry.register(Box::new(MyTool::new()));
```

---

## 📄 License

MIT
