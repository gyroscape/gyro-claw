//! # Agent Planner
//!
//! The core agent loop that orchestrates task execution.
//! Flow: User Input → Build Prompt → Call LLM → Parse Tool Call → Execute → Return Result
//!
//! The planner never has access to secrets. It only sees tool descriptions.
//!
//! Safety limits:
//! - max_iterations: maximum LLM round-trips (default 5)
//! - max_execution_time: total wall-clock time for the entire run (default 30s)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::agent::executor::{Executor, ToolResult};
use crate::agent::experience::{ExperienceEntry, ExperienceStore};
use crate::agent::memory::Memory;
use crate::agent::tool_parser::parse_tool_calls as parse_planner_tool_calls;
use crate::config::Config;
use crate::llm::client::LlmClient;
use crate::tools::ToolRegistry;

/// Represents a tool call parsed from the LLM response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Represents a message in the conversation.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String, // "system", "user", "assistant", "tool"
    pub content: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepResult {
    pub success: bool,
    pub error: Option<String>,
    pub retries: u8,
}

#[derive(Debug, Clone)]
#[derive(Default)]
pub struct PlannerRunOptions {
    pub task_id: Option<i64>,
    pub resume_checkpoint: Option<String>,
    pub progress_tx: Option<mpsc::UnboundedSender<PlannerProgressUpdate>>,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerProgressUpdate {
    pub phase: String,
    pub message: String,
    pub checkpoint: Option<String>,
}

#[derive(Debug)]
struct ToolExecutionOutcome {
    result: Result<Value>,
    step_result: StepResult,
    failure_fingerprint: Option<String>,
}

/// The Agent Planner orchestrates the agentic loop.
pub struct Planner {
    llm: LlmClient,
    executor: Executor,
    memory: Memory,
    experience: ExperienceStore,
    config: Config,
    system_prompt: String,
    /// Maximum number of LLM round-trips per run
    max_iterations: usize,
    /// Maximum wall-clock time for the entire run
    max_execution_time: Duration,
}

impl Planner {
    pub fn new(llm: LlmClient, executor: Executor, memory: Memory, config: Config) -> Self {
        let system_prompt = String::from(
            "You are Gyro-Claw, a local AI automation agent and an advanced software engineer. \
             You can use tools to accomplish tasks. \n\
             When you need to use a tool, respond with a JSON object in this format:\n\
             {\"tool\": \"<tool_name>\", \"args\": {<tool_arguments>}}\n\
             For independent read-only work, you may batch tool calls in this format:\n\
             {\"tools\": [{\"tool\": \"search\", \"args\": {...}}, {\"tool\": \"project_map\", \"args\": {...}}]}\n\
             When calling a tool, respond ONLY with JSON. Do not include explanations, markdown, tool-call tokens, or extra text before or after the JSON object.\n\
             When you have the final answer and have completed all actions, respond normally without JSON.\n\
             \n\
             # MANDATORY RULE — FILE CREATION:\n\
             If the user asks you to create, build, write, or generate ANYTHING, you MUST use the `edit` tool with action `create` to write it to disk inside `./workspace/`.\n\
             NEVER output raw code as your response. The user needs real files on disk, not printed text.\n\
             You are NOT done until every file exists on disk and you have verified it.\n\
             For multi-file projects, create ALL files — never skip any.\n\
             \n\
             # YOUR IDENTITY — PRODUCTION-GRADE SOFTWARE ENGINEER:\n\
             You are an elite full-stack engineer. Every output must be production-worthy — the kind of code shipped by Linear, Vercel, Stripe, and Supabase.\n\
             You handle EVERYTHING: frontend, backend, CLI tools, APIs, databases, DevOps, mobile, scripts.\n\
             \n\
             ## Project Setup (detect and adapt):\n\
             - **HTML/CSS/JS**: Write self-contained files with COMPLETE inline `<style>` and `<script>` blocks. Never leave CSS empty.\n\
             - **React/Next.js/Vite**: Scaffold with `shell` + `npx -y`, install shadcn/ui, create full component tree. For `create-*` or install commands, avoid hardcoded low timeouts (like 300). Omit `timeout_secs` or set it close to the configured shell max. If a command times out once, retry with a higher timeout before falling back to manual scaffolding.\n\
             - **Python/Flask/FastAPI/Django**: Create virtualenv, requirements.txt, proper project structure.\n\
             - **Rust/Go/Java**: Use cargo/go/gradle init, add deps, create proper module structure.\n\
             - **Any other**: Detect the stack, scaffold correctly, generate ALL config and source files.\n\
             \n\
             ## Code Quality (ALL languages):\n\
             - Write COMPLETE, WORKING code — never leave TODO placeholders or stub functions.\n\
             - Proper error handling (try/catch, Result types, error boundaries).\n\
             - Input validation and sanitization.\n\
             - Clean architecture: separation of concerns, single responsibility, DRY.\n\
             - Add comments only where logic is non-obvious.\n\
             - Follow language idioms (Pythonic Python, idiomatic Rust, modern JS/TS).\n\
             \n\
             ## Frontend/UI Quality:\n\
             When generating ANY user interface, it MUST look like a premium SaaS product:\n\
             - Dark theme with HSL color palette (no raw hex). Import Google Font Inter.\n\
             - Glassmorphism cards (backdrop-filter blur, rgba borders).\n\
             - Micro-animations: hover lift (translateY -2px), press scale(0.97), slideIn for list items, cubic-bezier transitions.\n\
             - Layered box-shadows for depth. Generous border-radius.\n\
             - Mobile-first responsive design (Flexbox/Grid).\n\
             - COMPLETE `<style>` block — EVERY element must be styled. Zero unstyled elements.\n\
             - For React/Next.js: use shadcn/ui + Tailwind. For vanilla: full CSS in `<style>`.\n\
             \n\
             ## Backend/API Quality:\n\
             - RESTful design with proper HTTP methods and status codes.\n\
             - Request validation, rate limiting, CORS configuration.\n\
             - Database migrations, connection pooling, proper ORM usage.\n\
             - Authentication/authorization patterns where appropriate.\n\
             - Structured logging, health check endpoints.\n\
             \n\
             ## Verification:\n\
             After creating files, ALWAYS verify by reading them back with `filesystem`. Run tests if applicable.\n\
             \n\
             # IMPORTANT BEHAVIORS:\n\
             1. **Never ask for or try to access passwords or secrets directly.** The system will inject required secrets automatically via {{vault:KEY}} placeholders.\n\
             2. **Core Agent Loop:** Always think in the sequence Observe -> Plan -> Act -> Verify -> Recover.\n\
             If a tool fails, times out, returns an invalid result, or leaves the UI unchanged, capture the failure, analyze it, and switch to an alternate strategy instead of repeating blindly.\n\
             2. **Iterative Problem Solving:** If fixing a bug or test, follow this strict loop:\n    - Run the test (via test_runner)\n    - Read the failing output and use search/filesystem to inspect the code\n    - Use edit to fix the code\n    - Run the test again to verify. Repeat until it passes.\n\
             3. Do not assume file contexts. Use project_map and filesystem to understand the project structure before editing files.\n\
             4. **Final Summary:** When you have completed the user's request, your final non-JSON response MUST conclude with a structured summary block matching this exact format:\n\
             \nTask completed: <brief goal description>\nChanges made:\n- <list of modified files and what was changed>\nTests (if applicable):\n✔ <passed count> passed\n✖ <failed count> failed\n\
             5. **Task Planning:** Before executing any sequence of tools for a new goal, first output a structured plan outlining the steps you will take, e.g., '1. Read X, 2. Search Y, 3. Edit Z'.\n\
             \n\
             # FILESYSTEM SANDBOX RULES:\n\
             All filesystem and edit tools are restricted to the workspace directory.\n\
             Allowed root: ./workspace\n\
             Any file you create or modify MUST use paths such as:\n\
             - ./workspace/file.txt\n\
             - ./workspace/output/result.md\n\
             If a tool error indicates a sandbox_violation, you MUST adjust the path to start with the workspace directory instead of falling into a loop.\n\
             \n\
             \n\
             # DESKTOP APPLICATION CONTROL\n\
             When operating desktop applications always follow:\n\
             1 check active app (using app_state)\n\
             2 capture screenshot (using screenshot)\n\
             3 detect UI elements (using ui_detector - prefer high confidence scores)\n\
             4 verify cursor position (using get_mouse_position) if needed for relative movement\n\
             5 interact with UI (using mouse or keyboard)\n\
             6 verify change (using screen_diff)\n\
             \n\
             IMPORTANT RULES FOR COMPUTER CONTROL:\n\
             1. Action Verification (CRITICAL): You MUST verify your actions. The correct loop is:\n\
                [screenshot] -> [detect target coordinates] -> [mouse/keyboard action] -> [NEW screenshot] -> [verify UI changed using screen_diff]\n\
             2. Self-Healing & Fallbacks: If `detect_ui_elements` cannot find the target or returns low confidence, DO NOT guess blind coordinates. Instead:\n\
                - Re-evaluate window target using `app_state`\n\
                - Use the `scroll` tool to scroll down or up if elements are out of bounds\n\
                - Capture a new screenshot and try again\n\
             3. Bounding Boxes / Coordinates (CRITICAL): When querying `app_state`, you will receive absolute window bounds. Normalize these bounds safely when clicking elements on multi-monitor setups based on the target display.\n\
             4. Limit your action speed. Think carefully before taking destructive actions.\n\
             \n\
             # BROWSER AUTOMATION RULES\n\
             When the goal involves interacting with websites, you MUST prioritize the `playwright` tool instead of mouse/keyboard automation.\n\
             Example workflow: playwright.open_url -> playwright.search -> playwright.click.\n\
             If `playwright` returns `service_unavailable`, do not retry it in the same run. Switch immediately to the local browser or computer-control workflow.\n\
             If using the local `browser` tool instead of playwright, you MUST follow this strictly deterministic workflow to ensure reliability:\n\
             1. **Vision-First Interaction (Mandatory):**\n\
             - screenshot → detect_ui_elements (with hint) → click center of bounding box → keyboard.type_text → keyboard.press_key enter.\n\
             2. **Selector Fallback Rule:** Use CSS selectors only if vision detection fails. Selector retries are strictly limited to 1 attempt per selector.\n\
             3. **No Selector Loops:** Never repeat the same selector action after it fails once. Switch strategy immediately.\n\
             4. **Screenshot Path:** Any browser screenshot you take will be saved to `./workspace/screenshots/browser_screenshot.png`.\n\
             5. **Data Extraction:** Use `extract` to retrieve the page title, links, and text content to build a plan before interacting.\n\
             \n\
             # VISION & STATE MACHINE RULES\n\
             You must strictly follow this interaction sequence:\n\
             Observe (screenshot) -> Decide (detect_ui_elements) -> Act (mouse/keyboard/browser) -> Verify (screenshot).\n\
             Never run detect_ui_elements more than 3 times in a row.\n\
             If detect_ui_elements fails twice, try clicking near the center of the screen and retry.\n\
             \n\
             # SKILLS SYSTEM\n\
             You have access to reusable skill playbooks via the `skills` tool.\n\
             Before starting a complex task, use `skills` with action `list` to check if a relevant skill exists.\n\
             If one matches, use `skills` with action `load` and the skill name to get step-by-step instructions, then follow them.\n\
             Skills provide battle-tested workflows for common tasks like scaffolding, deployment, and framework setup.\n\
             \n\
             # SUB-AGENT DELEGATION (CRITICAL)\n\
             For complex or multi-step tasks, you should delegate to specialized sub-agents to maintain security and focus:\n\
             1. **researcher_sub_agent**: Use for finding code, reading files, or searching the web. It is read-only and cannot mutate state.\n\
             2. **coder_sub_agent**: Use for making code changes, writing files, and running tests once you have a clear plan.\n\
             3. **browser_sub_agent**: Use for all external web interactions. It is isolated from your local filesystem.\n\
             Prefer delegation over performing long sequences of primitive tool calls yourself. This provides better isolation and state management.",
        );

        Self {
            llm,
            executor,
            memory: memory.clone(),
            experience: ExperienceStore::new(memory),
            max_iterations: config.max_iterations,
            max_execution_time: Duration::from_secs(config.execution.max_task_runtime_seconds),
            config,
            system_prompt,
        }
    }

    /// Adjust execution limits dynamically for intense tasks
    pub fn set_limits(&mut self, max_iterations: usize, max_execution_secs: u64) {
        self.max_iterations = max_iterations;
        self.max_execution_time = Duration::from_secs(max_execution_secs);
    }

    /// Run the agent loop for a single user command.
    /// The loop continues until the LLM produces a final answer,
    /// max iterations are reached, or max execution time is exceeded.
    pub async fn run(&mut self, user_input: &str, tool_registry: &ToolRegistry) -> Result<String> {
        self.run_with_options(user_input, tool_registry, PlannerRunOptions::default())
            .await
    }

    pub async fn run_with_options(
        &mut self,
        user_input: &str,
        tool_registry: &ToolRegistry,
        options: PlannerRunOptions,
    ) -> Result<String> {
        let start_time = Instant::now();
        let term = console::Term::stderr();

        // 1. Load project facts from memory and append to system prompt.
        let mut full_system_prompt = self.system_prompt.clone();
        if let Ok(facts_text) = self.memory.get_all_facts_text() {
            full_system_prompt.push_str(&facts_text);
        }

        let similar_experience = self
            .experience
            .find_similar_experience(user_input)
            .ok()
            .flatten();
        if let Some(experience) = similar_experience.as_ref() {
            full_system_prompt.push_str(&format!(
                "\nPrevious successful strategy: {}\n",
                Self::truncate_text(&experience.plan, 800)
            ));
        }

        // Save user message to memory.
        self.memory.add_conversation_entry("user", user_input).ok();

        let mut current_plan = self
            .generate_initial_plan(
                user_input,
                similar_experience.as_ref(),
                tool_registry,
                options.resume_checkpoint.as_deref(),
            )
            .await
            .unwrap_or_else(|_| {
                "1. Observe the current state.\n2. Use the minimal safe tools needed.\n3. Verify each action before continuing.\n4. Recover with an alternate strategy if a step fails.".to_string()
            });

        self.emit_progress(
            &options,
            "plan",
            format!("Initial plan ready for goal: {}", user_input),
            Some(self.build_checkpoint_payload(user_input, &current_plan, 0, &[], None)),
        );

        let mut conversation_history = vec![Message {
            role: "user".to_string(),
            content: Value::String(user_input.to_string()),
        }];
        let mut recent_tool_outputs: Vec<String> = Vec::new();
        let mut archived_context: Vec<String> = Vec::new();
        let mut context_summary: Option<String> = None;
        let mut summary_dirty = false;
        let mut tools_used: Vec<String> = Vec::new();
        let mut failure_fingerprints: HashMap<String, usize> = HashMap::new();

        struct UIRetryTracker {
            failed_detections: usize,
        }

        let mut ui_tracker = UIRetryTracker {
            failed_detections: 0,
        };

        struct FailureTracker {
            last_tool: String,
            last_error: String,
            repeat_count: usize,
        }

        let mut failure_tracker = FailureTracker {
            last_tool: String::new(),
            last_error: String::new(),
            repeat_count: 0,
        };

        let mut computer_actions_count = 0usize;
        let mut recent_actions: VecDeque<String> = VecDeque::new();
        let mut pending_navigation_wait = false;
        let mut consecutive_detect_ui = 0;
        let mut last_screen_width = 1920;
        let mut last_screen_height = 1080;
        let mut last_screenshot_path = String::new();
        let mut detected_elements_cache: HashMap<String, Value> = HashMap::new();
        let mut empty_detection_retries = 0;
        let mut playwright_unavailable = false;

        for iteration in 0..self.max_iterations {
            if iteration > 0 && iteration % 25 == 0 {
                if let Err(e) = self.memory.prune_memory(&self.config.memory) {
                    tracing::warn!("⚠️ Failed to prune memory: {}", e);
                }
            }

            // Check execution time limit.
            if start_time.elapsed() > self.max_execution_time {
                term.write_line("⚠️  Execution time limit exceeded.").ok();
                tracing::warn!(
                    "⏱️  Execution time limit exceeded ({:?}). Stopping agent loop.",
                    self.max_execution_time
                );
                return Ok(format!(
                    "⏱️  Execution time limit exceeded ({:.0}s). \
                     Stopping to prevent runaway execution. \
                     Here is what was accomplished so far.",
                    self.max_execution_time.as_secs_f64()
                ));
            }

            if summary_dirty {
                if let Some(summary) = self
                    .summarize_archived_context(context_summary.as_deref(), &archived_context)
                    .await
                {
                    context_summary = Some(summary);
                    archived_context.clear();
                    summary_dirty = false;
                }
            }

            let prompt_messages = self.build_iteration_messages(
                &full_system_prompt,
                &current_plan,
                tool_registry,
                &conversation_history,
                &recent_tool_outputs,
                context_summary.as_deref(),
            );

            term.write_line(&format!(
                "{} {}",
                console::style("🤔").cyan(),
                console::style("Thinking...").dim()
            ))
            .ok();

            self.emit_progress(
                &options,
                "observe",
                format!(
                    "Iteration {} planning against current context",
                    iteration + 1
                ),
                Some(self.build_checkpoint_payload(
                    user_input,
                    &current_plan,
                    iteration,
                    &recent_tool_outputs,
                    None,
                )),
            );

            // Call the LLM.
            let response = self
                .llm
                .chat(&prompt_messages)
                .await
                .context("Failed to get LLM response")?;

            if self.executor.llm_response_contains_secret(&response) {
                tracing::error!(
                    iteration = iteration + 1,
                    "blocking execution because model output contained resolved secret material"
                );
                return Err(anyhow::anyhow!(
                    "Security violation: model output contained resolved secret material."
                ));
            }

            // Try to parse as a tool call.
            let mut parsed_calls = parse_planner_tool_calls(&response);
            
            // Fallback: if parser returned empty but response likely contains a tool call,
            // try harder by finding the JSON substring starting with {"tool"
            if parsed_calls.is_empty() && (response.contains("\"tool\"") && response.contains("\"args\"")) {
                tracing::info!("Parser returned empty but response contains tool/args keywords, attempting fallback extraction");
                // Find the start of the JSON tool call
                if let Some(tool_start) = response.find("{\"tool\"")
                    .or_else(|| response.find("{ \"tool\""))
                    .or_else(|| response.find("{\\\"tool\\\""))
                {
                    let json_substr = &response[tool_start..];
                    // Try to parse from this point using serde_json which handles nested strings correctly
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_substr) {
                        if value.get("tool").is_some() {
                            let tool = value["tool"].as_str().unwrap_or("").to_string();
                            let args = value.get("args")
                                .or_else(|| value.get("arguments"))
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({}));
                            let normalized = crate::agent::tool_parser::resolve_tool_alias(
                                &crate::agent::tool_parser::normalize_tool_name(&tool)
                            ).to_string();
                            tracing::info!("Fallback extraction succeeded: tool={}", normalized);
                            parsed_calls = vec![crate::agent::tool_parser::ToolCall {
                                tool: normalized,
                                args,
                            }];
                        }
                    } else {
                        // serde_json failed — the JSON might be truncated or malformed.
                        // Try a streaming approach: read from tool_start, count braces manually
                        // accounting for string escapes
                        tracing::warn!("Fallback serde parse failed, trying manual brace extraction");
                        if let Some(extracted) = crate::agent::tool_parser::extract_json_object_from(json_substr) {
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(extracted) {
                                if value.get("tool").is_some() {
                                    let tool = value["tool"].as_str().unwrap_or("").to_string();
                                    let args = value.get("args")
                                        .or_else(|| value.get("arguments"))
                                        .cloned()
                                        .unwrap_or_else(|| serde_json::json!({}));
                                    let normalized = crate::agent::tool_parser::resolve_tool_alias(
                                        &crate::agent::tool_parser::normalize_tool_name(&tool)
                                    ).to_string();
                                    tracing::info!("Manual brace extraction succeeded: tool={}", normalized);
                                    parsed_calls = vec![crate::agent::tool_parser::ToolCall {
                                        tool: normalized,
                                        args,
                                    }];
                                }
                            }
                        }
                    }
                }
            }
            if !parsed_calls.is_empty() {
                conversation_history.push(Message {
                    role: "assistant".to_string(),
                    content: Value::String(response.clone()),
                });
                if Self::trim_conversation_messages(
                    &mut conversation_history,
                    &mut archived_context,
                ) {
                    summary_dirty = true;
                }

                if parsed_calls.len() > 1 {
                    let batch_calls = parsed_calls
                        .into_iter()
                        .map(|parsed_call| ToolCall {
                            tool_name: parsed_call.tool,
                            arguments: parsed_call.args,
                        })
                        .collect();
                    self.handle_parallel_batch(
                        user_input,
                        iteration,
                        &mut current_plan,
                        batch_calls,
                        tool_registry,
                        &mut recent_tool_outputs,
                        &mut archived_context,
                        &mut summary_dirty,
                        &mut tools_used,
                        &options,
                    )
                    .await?;
                    continue;
                }

                let parsed_call = parsed_calls.into_iter().next().unwrap();
                let tool_call = ToolCall {
                    tool_name: parsed_call.tool,
                    arguments: parsed_call.args,
                };
                tracing::info!(tool = %tool_call.tool_name, iteration = iteration + 1, "planner requested tool");
                term.clear_last_lines(1).ok();

                if tool_registry.get(&tool_call.tool_name).is_none() {
                    let suggestions = tool_registry.suggest_tools(&tool_call.tool_name, 3);
                    let unknown_tool_error = serde_json::json!({
                        "status": "error",
                        "error_type": "unknown_tool",
                        "requested_tool": tool_call.tool_name.clone(),
                        "message": "planner requested a tool that is not registered",
                        "suggestion": "Use one of the available tools listed in the system prompt.",
                        "did_you_mean": suggestions
                    });
                    let unknown_text =
                        serde_json::to_string(&unknown_tool_error).unwrap_or_else(|_| {
                            "{\"status\":\"error\",\"error_type\":\"unknown_tool\"}".to_string()
                        });
                    term.write_line(&format!(
                        "   ❌ {}",
                        console::style("Unknown tool requested by planner").red()
                    ))
                    .ok();
                    self.memory
                        .log_tool_execution("planner", "unknown_tool", &unknown_text, false)
                        .ok();
                    recent_tool_outputs
                        .push(format!("[Tool Result from 'planner']:\n{}", unknown_text));
                    if Self::trim_tool_outputs(&mut recent_tool_outputs, &mut archived_context) {
                        summary_dirty = true;
                    }
                    continue;
                }

                if self.is_computer_control_tool(&tool_call.tool_name) {
                    computer_actions_count += 1;
                    if computer_actions_count > self.config.computer_control.max_actions_per_cycle {
                        term.write_line(&format!(
                            "   ⚠️  {}",
                            console::style(
                                "Max computer actions per cycle reached. Aborting to prevent spam."
                            )
                            .red()
                        ))
                        .ok();
                        return Ok(format!("❌ Safety Intervention: Exceeded max allowed computer actions per cycle ({}). Task aborted.", self.config.computer_control.max_actions_per_cycle));
                    }
                }

                if tool_call.tool_name == "playwright" && playwright_unavailable {
                    let unavailable_text = serde_json::json!({
                        "status": "error",
                        "tool": "playwright",
                        "error_type": "service_unavailable",
                        "message": "playwright server is already marked unavailable for this run",
                        "suggestion": "Do not retry playwright. Use the local browser or computer-control tools instead."
                    })
                    .to_string();
                    term.write_line(&format!(
                        "   ⚠️  {}",
                        console::style("Skipping playwright because the service is unavailable")
                            .yellow()
                    ))
                    .ok();
                    self.memory
                        .log_tool_execution(
                            "playwright",
                            "service_unavailable",
                            &unavailable_text,
                            false,
                        )
                        .ok();
                    recent_tool_outputs.push(format!(
                        "[Tool Result from 'playwright']:\n{}",
                        unavailable_text
                    ));
                    if Self::trim_tool_outputs(&mut recent_tool_outputs, &mut archived_context) {
                        summary_dirty = true;
                    }
                    continue;
                }

                let action_signature = self.build_action_signature(&tool_call);
                recent_actions.push_back(action_signature.clone());
                if recent_actions.len() > 12 {
                    recent_actions.pop_front();
                }
                let repeated_actions = recent_actions
                    .iter()
                    .rev()
                    .take_while(|sig| *sig == &action_signature)
                    .count();

                if repeated_actions >= 3 {
                    term.write_line(&format!(
                        "   ⚠️  {}",
                        console::style("Detected repeated tool loop. Triggering recovery mode.")
                            .yellow()
                    ))
                    .ok();

                    let recovery_note = self.trigger_recovery_mode(tool_registry).await;
                    let recovery_output = format!(
                        "[Recovery Mode] Repeated action '{}' occurred {} times. {}",
                        action_signature, repeated_actions, recovery_note
                    );
                    recent_tool_outputs.push(recovery_output.clone());
                    if Self::trim_tool_outputs(&mut recent_tool_outputs, &mut archived_context) {
                        summary_dirty = true;
                    }
                    self.memory
                        .log_tool_execution("planner", "recovery_mode", &recovery_output, false)
                        .ok();
                    continue;
                }

                if tool_call.tool_name == "detect_ui_elements" && pending_navigation_wait {
                    term.write_line("   ⏳ Waiting 5 seconds for page load before UI detection...")
                        .ok();
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    pending_navigation_wait = false;
                }

                let mut execution_arguments = tool_call.arguments.clone();
                if tool_call.tool_name == "detect_ui_elements" {
                    execution_arguments = self
                        .prepare_detection_args_with_obstruction_handler(
                            &execution_arguments,
                            tool_registry,
                        )
                        .await;
                }

                term.write_line(&format!(
                    "⚙️  Running tool: {}",
                    console::style(&tool_call.tool_name).cyan().bold()
                ))
                .ok();
                let args_preview =
                    serde_json::to_string(&Self::compact_json_for_prompt(&execution_arguments, 0))
                        .unwrap_or_default();
                term.write_line(&format!("   {}", console::style(args_preview).dim()))
                    .ok();

                let mut block_vision_loop = false;
                if tool_call.tool_name == "detect_ui_elements" {
                    consecutive_detect_ui += 1;
                    if consecutive_detect_ui > 2 {
                        block_vision_loop = true;
                    }
                } else {
                    consecutive_detect_ui = 0;
                }

                if block_vision_loop {
                    let center_x = last_screen_width / 2;
                    let center_y = last_screen_height / 2;
                    let intercept_text = format!("{{\"status\":\"error\",\"error_type\":\"consecutive_vision_loop\",\"message\":\"[SYSTEM OVERRIDE] Too many consecutive detect_ui_elements calls. You MUST call an action (mouse/keyboard/scroll) now. If detection failed, fallback to clicking center of screen.\",\"suggestion\":\"mouse.click({}, {})\"}}", center_x, center_y);
                    term.write_line(&format!(
                        "   ⚠️  {}",
                        console::style("Intercepted consecutive detect_ui_elements limit").yellow()
                    ))
                    .ok();

                    let db_log_text =
                        "[Intercepted] Forced state transition to Action to prevent loop.";
                    self.memory
                        .log_tool_execution(
                            &tool_call.tool_name,
                            &execution_arguments.to_string(),
                            db_log_text,
                            false,
                        )
                        .ok();
                    recent_tool_outputs.push(format!(
                        "[Tool Result from '{}']:\n{}",
                        tool_call.tool_name, intercept_text
                    ));
                    if Self::trim_tool_outputs(&mut recent_tool_outputs, &mut archived_context) {
                        summary_dirty = true;
                    }
                    continue;
                }

                // Check Element Cache
                let mut cache_hit = false;
                if tool_call.tool_name == "detect_ui_elements" {
                    let image_path = execution_arguments
                        .get("image_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let hint = execution_arguments
                        .get("hint")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let cache_key = format!("{}_{}", image_path, hint);
                    if let Some(cached_result) = detected_elements_cache.get(&cache_key) {
                        term.write_line(&format!(
                            "   ⚡ {}",
                            console::style("Vision Cache Hit! Bypassing LLM.").green()
                        ))
                        .ok();
                        let result_text =
                            serde_json::to_string_pretty(cached_result).unwrap_or_default();
                        recent_tool_outputs.push(format!(
                            "[Tool Result from '{}']:\n{}",
                            tool_call.tool_name, result_text
                        ));
                        if Self::trim_tool_outputs(&mut recent_tool_outputs, &mut archived_context)
                        {
                            summary_dirty = true;
                        }
                        cache_hit = true;
                    }
                }

                if cache_hit {
                    continue;
                }

                // Execute the tool through the secure executor.
                let wait_start = Instant::now();
                let execution_outcome = self
                    .execute_tool_step(&tool_call.tool_name, &execution_arguments, tool_registry)
                    .await;
                let mut tool_result = execution_outcome.result;
                let mut step_result = execution_outcome.step_result;
                let mut failure_fingerprint = execution_outcome.failure_fingerprint;

                if self.result_has_error_type(&tool_result, "tool_call_limit") {
                    return Ok("⏱️  Local tool call limit reached. Returning partial results to prevent runaway task.".to_string());
                }

                // Optional: sandbox auto-correction retry logic.
                if self.result_has_error_type(&tool_result, "sandbox_violation") {
                    let attempted = tool_call
                        .arguments
                        .get("path")
                        .or_else(|| tool_call.arguments.get("file"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown_path");

                    self.memory
                        .log_tool_execution(
                            &tool_call.tool_name,
                            &format!("sandbox_violation on path: {}", attempted),
                            "workspace_root: ./workspace",
                            false,
                        )
                        .ok();

                    let mut new_args = execution_arguments.clone();
                    let mut did_retry = false;
                    if let Some(path) = execution_arguments.get("path").and_then(|p| p.as_str()) {
                        if !path.starts_with("./workspace") {
                            let corrected = format!("./workspace/{}", path.trim_start_matches('/'));
                            new_args["path"] = Value::String(corrected.clone());
                            term.write_line(&format!(
                                "   ⚠️  {}",
                                console::style(format!(
                                    "Sandbox violation. Auto-retrying with path: {}",
                                    corrected
                                ))
                                .yellow()
                            ))
                            .ok();
                            did_retry = true;
                        }
                    } else if let Some(file) =
                        execution_arguments.get("file").and_then(|f| f.as_str())
                    {
                        if !file.starts_with("./workspace") {
                            let corrected = format!("./workspace/{}", file.trim_start_matches('/'));
                            new_args["file"] = Value::String(corrected.clone());
                            term.write_line(&format!(
                                "   ⚠️  {}",
                                console::style(format!(
                                    "Sandbox violation. Auto-retrying with file: {}",
                                    corrected
                                ))
                                .yellow()
                            ))
                            .ok();
                            did_retry = true;
                        }
                    }

                    if did_retry {
                        tool_result = self
                            .executor
                            .execute(&tool_call.tool_name, new_args, tool_registry)
                            .await;
                        step_result.retries = step_result.retries.saturating_add(1);
                    }
                }

                if tool_call.tool_name == "detect_ui_elements"
                    && self.result_has_error_type(&tool_result, "ui_not_found")
                {
                    term.write_line(
                        "   ⚠️  UI not found. Capturing a fresh screenshot and retrying once.",
                    )
                    .ok();
                    if let Some(retry_value) = self
                        .retry_ui_detection_with_new_screenshot(&execution_arguments, tool_registry)
                        .await
                    {
                        tool_result = Ok(retry_value);
                        step_result.retries = step_result.retries.saturating_add(1);
                    }
                }

                if tool_call.tool_name == "browser"
                    && tool_call.arguments.get("action").and_then(|v| v.as_str())
                        == Some("navigate")
                    && matches!(&tool_result, Ok(value) if !Self::is_error_response(value))
                {
                    pending_navigation_wait = true;
                }

                // SECURITY FIX: Do NOT extend max_execution_time by tool wait duration.
                // The original code made the timeout effectively infinite by growing the
                // deadline after every tool call. Execution time must be absolute wall-clock.
                let _wait_duration = wait_start.elapsed();

                let mut success = false;
                let mut error_type: Option<String> = None;
                let mut error_message = String::new();
                let mut result_text: String;
                let mut result_value_for_log: Option<Value> = None;

                match tool_result {
                    Ok(value) => {
                        let sanitized = self.sanitize_tool_result(&tool_call.tool_name, &value);
                        result_value_for_log = Some(sanitized.clone());

                        if Self::is_error_response(&value)
                            || self.is_invalid_tool_result(&tool_call.tool_name, &value)
                        {
                            error_type = value
                                .get("error_type")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    self.is_invalid_tool_result(&tool_call.tool_name, &value)
                                        .then_some("invalid_result".to_string())
                                });
                            error_message = value
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or_else(|| {
                                    if self.is_invalid_tool_result(&tool_call.tool_name, &value) {
                                        "tool returned an invalid result"
                                    } else {
                                        "tool returned structured error"
                                    }
                                })
                                .to_string();
                            step_result.success = false;
                            step_result.error = Some(error_message.clone());
                            failure_fingerprint.get_or_insert_with(|| {
                                self.build_failure_fingerprint(
                                    &tool_call.tool_name,
                                    error_type.as_deref().unwrap_or("invalid_result"),
                                    &error_message,
                                )
                            });
                            result_text = format!("Tool execution error: {}", error_message);
                            term.write_line(&format!(
                                "   ❌ {}",
                                console::style(format!("Failed: {}", error_message)).red()
                            ))
                            .ok();
                        } else {
                            success = true;
                            step_result.success = true;
                            result_text =
                                serde_json::to_string_pretty(&sanitized).unwrap_or_default();
                            term.write_line(&format!(
                                "   ✅ {}",
                                console::style("Success").green()
                            ))
                            .ok();
                        }
                    }
                    Err(err) => {
                        error_message = err.to_string();
                        step_result.success = false;
                        step_result.error = Some(error_message.clone());
                        failure_fingerprint.get_or_insert_with(|| {
                            self.build_failure_fingerprint(
                                &tool_call.tool_name,
                                "executor_error",
                                &error_message,
                            )
                        });
                        result_text = format!("Tool execution error: {}", error_message);
                        term.write_line(&format!(
                            "   ❌ {}",
                            console::style(format!("Failed: {}", error_message)).red()
                        ))
                        .ok();
                    }
                }

                // SECURITY FIX: Replaced .unwrap() calls with safe pattern matching
                // to prevent runtime panics that could crash the agent.
                if success && tool_call.tool_name == "screenshot" {
                    if let Some(val) = result_value_for_log.as_ref() {
                        if let Some(width) = val.get("width").and_then(|v| v.as_i64()) {
                            last_screen_width = width as i32;
                        }
                        if let Some(height) = val.get("height").and_then(|v| v.as_i64()) {
                            last_screen_height = height as i32;
                        }
                        if let Some(path) = val.get("image_path").and_then(|v| v.as_str()) {
                            last_screenshot_path = path.to_string();
                        }
                    }
                }

                if success && tool_call.tool_name == "detect_ui_elements" {
                    let image_path = execution_arguments
                        .get("image_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let hint = execution_arguments
                        .get("hint")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let cache_key = format!("{}_{}", image_path, hint);
                    // SECURITY FIX: Safe access instead of unwrap() to prevent panic.
                    if let Some(val) = result_value_for_log.as_ref() {
                        detected_elements_cache.insert(cache_key, val.clone());
                    }

                    // --- FORCE AUTO-ACTION BLOCK ---
                    // If detection succeeded, we force an immediate action so the LLM doesn't just loop on detection again.
                    let mut elements_found = false;
                    if let Some(res_val) = &result_value_for_log {
                        if let Some(elements) = res_val.get("elements").and_then(|e| e.as_array()) {
                            if !elements.is_empty() {
                                elements_found = true;
                                if let Some(first_el) = elements.first() {
                                    if let Some(click_pt) =
                                        first_el.get("click_point").and_then(|cp| cp.as_array())
                                    {
                                        if click_pt.len() >= 2 {
                                            if let (Some(x), Some(y)) =
                                                (click_pt[0].as_i64(), click_pt[1].as_i64())
                                            {
                                                term.write_line(&format!("   🖱️  {}", console::style(format!("Auto-clicking detected element at ({}, {})", x, y)).green())).ok();

                                                // Execute the click immediately
                                                let click_args = serde_json::json!({
                                                    "action": "click",
                                                    "x": x,
                                                    "y": y
                                                });

                                                let _ = self
                                                    .executor
                                                    .execute("mouse", click_args, tool_registry)
                                                    .await;

                                                result_text.push_str(&format!("\n\n[SYSTEM] Automatically clicked the detected element at ({}, {}). If this element requires text input, use the 'keyboard' tool next.", x, y));
                                                empty_detection_retries = 0;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !elements_found {
                        empty_detection_retries += 1;
                        if empty_detection_retries >= 3 {
                            return Ok("❌ Task aborted: Empty detection limit (3) exceeded. Could not find any elements to act upon.".to_string());
                        }

                        let center_x = last_screen_width / 2;
                        let center_y = last_screen_height / 2;
                        term.write_line(&format!("   ⚠️  {}", console::style(format!("0 elements found. Auto-clicking center ({}, {}) and re-evaluating.", center_x, center_y)).yellow())).ok();

                        let click_args = serde_json::json!({
                            "action": "click",
                            "x": center_x,
                            "y": center_y
                        });
                        let _ = self
                            .executor
                            .execute("mouse", click_args, tool_registry)
                            .await;

                        tokio::time::sleep(Duration::from_millis(1500)).await;

                        if tool_registry.get("screenshot").is_some() {
                            if let Ok(screenshot_val) = self
                                .executor
                                .execute("screenshot", serde_json::json!({}), tool_registry)
                                .await
                            {
                                if !Self::is_error_response(&screenshot_val) {
                                    let new_path = screenshot_val
                                        .get("image_path")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default()
                                        .to_string();
                                    last_screenshot_path = new_path;
                                    let formatted = self.format_tool_output_for_prompt(
                                        "screenshot",
                                        &screenshot_val,
                                    );
                                    result_text.push_str(&format!("\n\n[SYSTEM] 0 elements found. Automatically clicked center of screen ({}, {}) to attempt focus. New screenshot state:\n{}", center_x, center_y, formatted));
                                }
                            }
                        }
                    }
                    // --- END FORCE AUTO-ACTION BLOCK ---
                }

                if success
                    && (tool_call.tool_name == "mouse"
                        || tool_call.tool_name == "keyboard"
                        || tool_call.tool_name == "scroll")
                {
                    term.write_line(&format!(
                        "   📸 {}",
                        console::style("Auto-verifying action with screenshot & diff...").dim()
                    ))
                    .ok();
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    if tool_registry.get("screenshot").is_some()
                        && tool_registry.get("screen_diff").is_some()
                    {
                        let prev_screenshot_path = last_screenshot_path.clone();

                        if let Ok(screenshot_val) = self
                            .executor
                            .execute("screenshot", serde_json::json!({}), tool_registry)
                            .await
                        {
                            if !Self::is_error_response(&screenshot_val) {
                                let new_path = screenshot_val
                                    .get("image_path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();

                                if let Some(width) =
                                    screenshot_val.get("width").and_then(|v| v.as_i64())
                                {
                                    last_screen_width = width as i32;
                                }
                                if let Some(height) =
                                    screenshot_val.get("height").and_then(|v| v.as_i64())
                                {
                                    last_screen_height = height as i32;
                                }
                                last_screenshot_path = new_path.clone();

                                let formatted = self
                                    .format_tool_output_for_prompt("screenshot", &screenshot_val);
                                result_text.push_str(&format!("\n\n[Auto-Verify] Automatic screenshot taken after action:\n{}", formatted));

                                if !prev_screenshot_path.is_empty() && !new_path.is_empty() {
                                    if let Ok(diff_val) = self.executor.execute("screen_diff", serde_json::json!({"image1": prev_screenshot_path, "image2": new_path}), tool_registry).await {
                                        if !Self::is_error_response(&diff_val) {
                                            let changed = diff_val.get("changed").and_then(|v| v.as_bool()).unwrap_or(false);
                                            let percent = diff_val.get("difference_percent").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                            result_text.push_str(&format!("\n\n[Auto-Verify Diff] Screen changed: {}, Difference: {:.2}%", changed, percent));
                                            if !changed {
                                                success = false;
                                                step_result.success = false;
                                                error_type = Some("unchanged_ui_state".to_string());
                                                error_message = format!(
                                                    "{} did not change the UI state",
                                                    tool_call.tool_name
                                                );
                                                step_result.error = Some(error_message.clone());
                                                failure_fingerprint = Some(self.build_failure_fingerprint(
                                                    &tool_call.tool_name,
                                                    "unchanged_ui_state",
                                                    &error_message,
                                                ));
                                                result_text.push_str(
                                                    "\n\n[Auto-Verify] UI state was unchanged. Trigger recovery and choose a different action."
                                                );
                                                term.write_line(&format!(
                                                    "   ⚠️  {}",
                                                    console::style("UI unchanged after action").yellow()
                                                )).ok();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if success {
                    tools_used.push(tool_call.tool_name.clone());
                    failure_tracker.repeat_count = 0;
                    failure_tracker.last_error.clear();
                    failure_tracker.last_tool.clear();
                    ui_tracker.failed_detections = 0;
                    step_result.success = true;
                    step_result.error = None;
                } else {
                    let current_error = if error_message.is_empty() {
                        result_text.clone()
                    } else {
                        error_message.clone()
                    };
                    let current_tool = tool_call.tool_name.clone();

                    if failure_tracker.last_error == current_error
                        && failure_tracker.last_tool == current_tool
                    {
                        failure_tracker.repeat_count += 1;
                    } else {
                        failure_tracker.last_error = current_error.clone();
                        failure_tracker.last_tool = current_tool.clone();
                        failure_tracker.repeat_count = 1;
                    }

                    if failure_tracker.repeat_count >= self.config.max_retries.max(1) {
                        self.memory
                            .log_tool_execution(
                                "planner",
                                "strategy_check",
                                "Loop detected after repeated failures. Forced strategy shift.",
                                false,
                            )
                            .ok();
                        let intervention = "\n[SYSTEM OVERRIDE] You have failed 3 or more times consecutively with the same tool/error. Stop repeating. Re-analyze UI/context and choose an alternate strategy.";
                        result_text.push_str(intervention);
                        term.write_line(&format!(
                            "   ⚠️  {}",
                            console::style("Strategy Intervention Triggered").yellow()
                        ))
                        .ok();
                    }

                    if let Some(fingerprint) = failure_fingerprint.as_ref() {
                        let repeat_count = failure_fingerprints
                            .entry(fingerprint.clone())
                            .and_modify(|count| *count += 1)
                            .or_insert(1usize);

                        if *repeat_count > self.config.max_retries.max(1) {
                            tracing::error!(
                                tool = %current_tool,
                                fingerprint = %fingerprint,
                                repeats = *repeat_count,
                                "aborting due to repeated identical failures"
                            );
                            return Ok(format!(
                                "❌ Task aborted after repeated identical failure while using '{}': {}",
                                current_tool, current_error
                            ));
                        }
                    }

                    if current_tool == "detect_ui_elements"
                        || current_tool == "screen_diff"
                        || error_type.as_deref() == Some("ui_not_found")
                    {
                        ui_tracker.failed_detections += 1;
                        if ui_tracker.failed_detections >= 5 {
                            term.write_line(&format!(
                                "   ❌ {}",
                                console::style("Max UI detection attempts exceeded").red()
                            ))
                            .ok();
                            return Ok("❌ Task aborted: Max UI detection attempts (5) exceeded. The target UI element could not be reliably located.".to_string());
                        } else if ui_tracker.failed_detections >= 2 {
                            let center_x = last_screen_width / 2;
                            let center_y = last_screen_height / 2;
                            let ui_intervention = format!(
                                "\n[SYSTEM OVERRIDE] UI detection has failed {} times. Take a new screenshot, OR try clicking near the center of the screen `mouse.click(x: {}, y: {})` to trigger focus and retry.",
                                ui_tracker.failed_detections, center_x, center_y
                            );
                            result_text.push_str(&ui_intervention);
                            term.write_line(&format!(
                                "   ⚠️  {}",
                                console::style(format!(
                                    "Suggesting center click fallback ({}, {})",
                                    center_x, center_y
                                ))
                                .magenta()
                            ))
                            .ok();
                        }
                    }

                    if current_tool == "playwright"
                        && error_type.as_deref() == Some("service_unavailable")
                    {
                        playwright_unavailable = true;
                        result_text.push_str(
                            "\n[SYSTEM] Playwright service is unavailable for this run. Do not retry it; switch to local browser or computer-control tools.",
                        );
                    }

                    let recovery_strategy = self
                        .generate_recovery_plan(
                            user_input,
                            &current_plan,
                            &tool_call.tool_name,
                            &current_error,
                            error_type.as_deref(),
                        )
                        .await;
                    current_plan = recovery_strategy.clone();
                    result_text.push_str(&format!(
                        "\n\n[Recovery]\nPrevious step result: {:?}\nNew plan:\n{}",
                        step_result, recovery_strategy
                    ));
                }

                let db_log_text = if let Some(value) = result_value_for_log.as_ref() {
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| result_text.clone())
                } else {
                    Self::truncate_text(&result_text, 3000)
                };

                // Log tool execution in memory.
                self.memory
                    .log_tool_execution(
                        &tool_call.tool_name,
                        &execution_arguments.to_string(),
                        &db_log_text,
                        success,
                    )
                    .ok();

                if let Some(value) = result_value_for_log.as_ref() {
                    recent_tool_outputs
                        .push(self.format_tool_output_for_prompt(&tool_call.tool_name, value));
                } else {
                    recent_tool_outputs.push(format!(
                        "[Tool Result from '{}']:\n{}",
                        tool_call.tool_name,
                        Self::truncate_text(&result_text, 900)
                    ));
                }
                if Self::trim_tool_outputs(&mut recent_tool_outputs, &mut archived_context) {
                    summary_dirty = true;
                }
                self.emit_progress(
                    &options,
                    if success { "verify" } else { "recover" },
                    format!(
                        "{} {}",
                        if success {
                            "Completed tool"
                        } else {
                            "Recovering from tool failure in"
                        },
                        tool_call.tool_name
                    ),
                    Some(self.build_checkpoint_payload(
                        user_input,
                        &current_plan,
                        iteration,
                        &recent_tool_outputs,
                        if success {
                            None
                        } else {
                            Some(error_message.as_str())
                        },
                    )),
                );
            } else {
                term.clear_last_lines(1).ok();
                term.write_line(&format!(
                    "{} {}",
                    console::style("✅").green(),
                    console::style("Final response ready").dim()
                ))
                .ok();
                
                // Print the actual response so the user sees why it stopped
                if !response.trim().is_empty() {
                    term.write_line("\n--- Agent Response ---").ok();
                    term.write_line(response.trim()).ok();
                    term.write_line("----------------------").ok();
                } else {
                    term.write_line("\n[Agent returned an empty response]").ok();
                }

                self.memory
                    .add_conversation_entry("assistant", &response)
                    .ok();

                self.experience
                    .store_experience(user_input, &current_plan, &tools_used, &response)
                    .ok();
                self.emit_progress(
                    &options,
                    "completed",
                    format!("Task completed for goal: {}", user_input),
                    Some(self.build_checkpoint_payload(
                        user_input,
                        &current_plan,
                        iteration,
                        &recent_tool_outputs,
                        None,
                    )),
                );

                return Ok(response);
            }
        }

        let fallback = format!(
            "⚠️  Reached maximum iterations ({}) without a final answer. \
             Total time: {:.1}s. Please review the partial results above.",
            self.max_iterations,
            start_time.elapsed().as_secs_f64()
        );
        self.emit_progress(
            &options,
            "failed",
            format!("Planner hit max iterations for goal: {}", user_input),
            Some(self.build_checkpoint_payload(
                user_input,
                &current_plan,
                self.max_iterations,
                &recent_tool_outputs,
                Some("max_iterations_reached"),
            )),
        );
        Ok(fallback)
    }

    fn build_iteration_messages(
        &self,
        full_system_prompt: &str,
        current_plan: &str,
        tool_registry: &ToolRegistry,
        conversation_history: &[Message],
        recent_tool_outputs: &[String],
        context_summary: Option<&str>,
    ) -> Vec<Message> {
        let mut system_prompt = self.build_system_prompt(full_system_prompt, tool_registry);
        system_prompt.push_str("\n\nCurrent execution plan:\n");
        system_prompt.push_str(current_plan);
        if let Some(summary) = context_summary {
            system_prompt.push_str("\n\nCondensed earlier context:\n");
            system_prompt.push_str(summary);
        }

        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Value::String(system_prompt),
        }];

        for msg in conversation_history
            .iter()
            .filter(|m| m.role == "user")
            .rev()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            messages.push(msg);
        }

        for output in recent_tool_outputs
            .iter()
            .rev()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            messages.push(Message {
                role: "user".to_string(),
                content: Value::String(output),
            });
        }

        messages
    }

    fn trim_conversation_messages(
        conversation_history: &mut Vec<Message>,
        archived_context: &mut Vec<String>,
    ) -> bool {
        let mut trimmed = false;
        while conversation_history.len() > 3 {
            let removed = conversation_history.remove(0);
            let content = match removed.content {
                Value::String(text) => text,
                other => serde_json::to_string(&Self::compact_json_for_prompt(&other, 0))
                    .unwrap_or_else(|_| String::from("[unserializable_message]")),
            };
            archived_context.push(format!(
                "{}: {}",
                removed.role,
                Self::truncate_text(&content, 500)
            ));
            trimmed = true;
        }
        trimmed
    }

    fn trim_tool_outputs(outputs: &mut Vec<String>, archived_context: &mut Vec<String>) -> bool {
        let mut trimmed = false;
        while outputs.len() > 5 {
            let removed = outputs.remove(0);
            archived_context.push(format!(
                "tool_output: {}",
                Self::truncate_text(&removed, 500)
            ));
            trimmed = true;
        }
        trimmed
    }

    async fn summarize_archived_context(
        &self,
        existing_summary: Option<&str>,
        archived_context: &[String],
    ) -> Option<String> {
        if archived_context.is_empty() {
            return existing_summary.map(|s| s.to_string());
        }

        let mut payload = String::new();
        if let Some(summary) = existing_summary {
            payload.push_str("Existing summary:\n");
            payload.push_str(&Self::truncate_text(summary, 1200));
            payload.push_str("\n\n");
        }

        payload.push_str("New archived context:\n");
        let mut consumed = 0usize;
        for item in archived_context.iter().rev().take(40).rev() {
            let clipped = Self::truncate_text(item, 240);
            consumed += clipped.len();
            if consumed > 4500 {
                break;
            }
            payload.push_str("- ");
            payload.push_str(&clipped);
            payload.push('\n');
        }

        let summary_messages = vec![
            Message {
                role: "system".to_string(),
                content: Value::String(
                    "Summarize the archived agent context into concise bullets. Keep unresolved goals, key errors, attempted strategies, and critical observations. Omit verbose logs."
                        .to_string(),
                ),
            },
            Message {
                role: "user".to_string(),
                content: Value::String(payload),
            },
        ];

        match self.llm.chat(&summary_messages).await {
            Ok(summary) => Some(Self::truncate_text(summary.trim(), 1800)),
            Err(err) => {
                tracing::warn!("Failed to summarize archived context: {}", err);
                existing_summary.map(|s| s.to_string())
            }
        }
    }

    fn sanitize_tool_result(&self, tool_name: &str, value: &Value) -> Value {
        let mut compact = Self::compact_json_for_prompt(value, 0);
        if tool_name == "screenshot" {
            if let Some(obj) = compact.as_object_mut() {
                obj.remove("base64");
                obj.remove("image_base64");
                if let Some(path) = obj.get("image_path").and_then(|v| v.as_str()) {
                    let file_name = Path::new(path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path)
                        .to_string();
                    obj.insert("image_path".to_string(), Value::String(file_name));
                }
            }
        }
        compact
    }

    fn format_tool_output_for_prompt(&self, tool_name: &str, value: &Value) -> String {
        if tool_name == "screenshot" && !Self::is_error_response(value) {
            let file_name = value
                .get("image_path")
                .and_then(|v| v.as_str())
                .map(|path| {
                    Path::new(path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path)
                        .to_string()
                })
                .unwrap_or_else(|| "unknown_screenshot.png".to_string());
            return format!(
                "[Tool Result from 'screenshot']:\n{{\"status\":\"ok\",\"file\":\"{}\"}}",
                file_name
            );
        }

        let compact = Self::compact_json_for_prompt(value, 0);
        let payload = serde_json::to_string(&compact).unwrap_or_else(|_| "{}".to_string());
        format!("[Tool Result from '{}']:\n{}", tool_name, payload)
    }

    fn compact_json_for_prompt(value: &Value, depth: usize) -> Value {
        if depth > 4 {
            return Value::String("[truncated]".to_string());
        }

        match value {
            Value::String(s) => Value::String(Self::truncate_text(s, 400)),
            Value::Array(arr) => Value::Array(
                arr.iter()
                    .take(20)
                    .map(|v| Self::compact_json_for_prompt(v, depth + 1))
                    .collect(),
            ),
            Value::Object(map) => {
                let mut new_map = serde_json::Map::new();
                for (idx, (key, val)) in map.iter().enumerate() {
                    if idx >= 40 {
                        new_map.insert(
                            "_truncated".to_string(),
                            Value::String("additional fields omitted".to_string()),
                        );
                        break;
                    }
                    if key.eq_ignore_ascii_case("base64")
                        || key.eq_ignore_ascii_case("image_base64")
                    {
                        continue;
                    }
                    if key == "image_path" {
                        if let Some(path) = val.as_str() {
                            let file_name = Path::new(path)
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or(path)
                                .to_string();
                            new_map.insert(key.clone(), Value::String(file_name));
                            continue;
                        }
                    }
                    new_map.insert(key.clone(), Self::compact_json_for_prompt(val, depth + 1));
                }
                Value::Object(new_map)
            }
            other => other.clone(),
        }
    }

    fn truncate_text(text: &str, max_chars: usize) -> String {
        if text.chars().count() <= max_chars {
            text.to_string()
        } else {
            let truncated: String = text.chars().take(max_chars).collect();
            format!("{}... [TRUNCATED]", truncated)
        }
    }

    fn is_error_response(value: &Value) -> bool {
        let status_error = value
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("error"))
            .unwrap_or(false);
        status_error || value.get("error").is_some()
    }

    fn result_has_error_type(&self, result: &Result<Value>, expected_error_type: &str) -> bool {
        match result {
            Ok(value) => value
                .get("error_type")
                .and_then(|v| v.as_str())
                .map(|s| s == expected_error_type)
                .unwrap_or(false),
            Err(err) => err.to_string().contains(expected_error_type),
        }
    }

    fn is_computer_control_tool(&self, tool_name: &str) -> bool {
        matches!(
            tool_name,
            "screenshot"
                | "mouse"
                | "keyboard"
                | "system"
                | "screen_diff"
                | "detect_ui_elements"
                | "window"
                | "app_state"
                | "get_mouse_position"
                | "scroll"
        )
    }

    fn build_action_signature(&self, call: &ToolCall) -> String {
        let compact_args = Self::compact_json_for_prompt(&call.arguments, 0);
        let args_json = serde_json::to_string(&compact_args).unwrap_or_default();
        format!(
            "{}::{}",
            call.tool_name,
            Self::truncate_text(&args_json, 240)
        )
    }

    async fn trigger_recovery_mode(&self, tool_registry: &ToolRegistry) -> String {
        let mut notes = Vec::new();

        let screenshot_res = self
            .executor
            .execute("screenshot", serde_json::json!({}), tool_registry)
            .await;

        match screenshot_res {
            Ok(screenshot_val) if !Self::is_error_response(&screenshot_val) => {
                let image_path = screenshot_val
                    .get("image_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let screenshot_file = Path::new(&image_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown.png");
                notes.push(format!(
                    "captured recovery screenshot '{}'",
                    screenshot_file
                ));

                if tool_registry.get("detect_ui_elements").is_some() && !image_path.is_empty() {
                    let ui_scan_res = self
                        .executor
                        .execute(
                            "detect_ui_elements",
                            serde_json::json!({
                                "image_path": image_path,
                                "hint": "Re-analyze current UI and identify alternate clickable targets."
                            }),
                            tool_registry,
                        )
                        .await;

                    match ui_scan_res {
                        Ok(scan_val) if !Self::is_error_response(&scan_val) => {
                            let count = scan_val
                                .get("elements")
                                .and_then(|v| v.as_array())
                                .map(|arr| arr.len())
                                .unwrap_or(0);
                            notes.push(format!(
                                "reanalyzed UI and found {} element candidates",
                                count
                            ));
                        }
                        Ok(scan_val) => {
                            let message = scan_val
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("ui re-analysis failed");
                            notes.push(format!(
                                "ui re-analysis failed: {}",
                                Self::truncate_text(message, 120)
                            ));
                        }
                        Err(err) => notes.push(format!(
                            "ui re-analysis error: {}",
                            Self::truncate_text(&err.to_string(), 120)
                        )),
                    }
                }
            }
            Ok(err_val) => {
                let message = err_val
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("screenshot failed during recovery");
                notes.push(format!(
                    "recovery screenshot failed: {}",
                    Self::truncate_text(message, 120)
                ));
            }
            Err(err) => notes.push(format!(
                "recovery screenshot error: {}",
                Self::truncate_text(&err.to_string(), 120)
            )),
        }

        notes.push("choose an alternate strategy instead of repeating the same action".to_string());
        notes.join(" | ")
    }

    async fn retry_ui_detection_with_new_screenshot(
        &self,
        original_args: &Value,
        tool_registry: &ToolRegistry,
    ) -> Option<Value> {
        if tool_registry.get("screenshot").is_none()
            || tool_registry.get("detect_ui_elements").is_none()
        {
            return None;
        }

        let screenshot_res = self
            .executor
            .execute("screenshot", serde_json::json!({}), tool_registry)
            .await
            .ok()?;

        if Self::is_error_response(&screenshot_res) {
            return Some(screenshot_res);
        }

        let image_path = screenshot_res
            .get("image_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())?;
        let hint = original_args
            .get("hint")
            .and_then(|v| v.as_str())
            .unwrap_or("target UI element");
        let refreshed_args = self
            .prepare_detection_args_with_obstruction_handler(
                &serde_json::json!({
                    "image_path": image_path,
                    "hint": hint
                }),
                tool_registry,
            )
            .await;

        self.executor
            .execute("detect_ui_elements", refreshed_args, tool_registry)
            .await
            .ok()
    }

    async fn prepare_detection_args_with_obstruction_handler(
        &self,
        args: &Value,
        tool_registry: &ToolRegistry,
    ) -> Value {
        let Some(image_path) = args.get("image_path").and_then(|v| v.as_str()) else {
            return args.clone();
        };

        let cleared_image_path = self
            .dismiss_ui_obstructions(image_path, tool_registry)
            .await;
        if cleared_image_path == image_path {
            return args.clone();
        }

        let mut updated_args = args.clone();
        updated_args["image_path"] = Value::String(cleared_image_path);
        updated_args
    }

    async fn dismiss_ui_obstructions(
        &self,
        image_path: &str,
        tool_registry: &ToolRegistry,
    ) -> String {
        const MAX_OVERLAY_CHECKS: usize = 2;
        const OVERLAY_HINTS: [&str; 5] = [
            "Accept all",
            "I agree",
            "Agree",
            "Accept cookies",
            "Reject all",
        ];

        if image_path.trim().is_empty()
            || tool_registry.get("screenshot").is_none()
            || tool_registry.get("detect_ui_elements").is_none()
            || tool_registry.get("mouse").is_none()
        {
            return image_path.to_string();
        }

        let mut current_image_path = image_path.to_string();

        for attempt in 0..MAX_OVERLAY_CHECKS {
            let mut overlay_clicked = false;

            for hint in OVERLAY_HINTS {
                let detection = match self
                    .executor
                    .execute(
                        "detect_ui_elements",
                        serde_json::json!({
                            "image_path": current_image_path,
                            "hint": hint
                        }),
                        tool_registry,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::debug!("UI obstruction check failed for hint '{}': {}", hint, err);
                        continue;
                    }
                };

                if Self::is_error_response(&detection) {
                    continue;
                }

                let Some((center_x, center_y, bbox)) =
                    Self::extract_bbox_center_from_detection(&detection)
                else {
                    continue;
                };

                tracing::info!(
                    "UI obstruction handler dismissed overlay using hint '{}' on attempt {}",
                    hint,
                    attempt + 1
                );

                let click_result = match self
                    .executor
                    .execute(
                        "mouse",
                        serde_json::json!({
                            "action": "click",
                            "x": center_x,
                            "y": center_y,
                            "bounding_box": bbox
                        }),
                        tool_registry,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::debug!("UI obstruction click failed for hint '{}': {}", hint, err);
                        continue;
                    }
                };

                if Self::is_error_response(&click_result) {
                    continue;
                }

                self.wait_after_overlay_dismiss(tool_registry).await;

                let screenshot_result = match self
                    .executor
                    .execute("screenshot", serde_json::json!({}), tool_registry)
                    .await
                {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::debug!(
                            "UI obstruction recapture failed after hint '{}': {}",
                            hint,
                            err
                        );
                        overlay_clicked = true;
                        break;
                    }
                };

                if let Some(updated_image_path) =
                    Self::extract_image_path_from_tool_result(&screenshot_result)
                {
                    current_image_path = updated_image_path;
                }

                overlay_clicked = true;
                break;
            }

            if !overlay_clicked {
                break;
            }
        }

        current_image_path
    }

    async fn wait_after_overlay_dismiss(&self, tool_registry: &ToolRegistry) {
        if tool_registry.get("wait_for").is_some() {
            let _ = self
                .executor
                .execute(
                    "wait_for",
                    serde_json::json!({ "timeout": 2 }),
                    tool_registry,
                )
                .await;
        } else {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    fn extract_image_path_from_tool_result(value: &Value) -> Option<String> {
        value
            .get("image_path")
            .and_then(|v| v.as_str())
            .or_else(|| {
                value
                    .get("result")
                    .and_then(|result| result.get("image_path"))
                    .and_then(|v| v.as_str())
            })
            .map(|path| path.to_string())
    }

    fn is_selector_based_browser_args(&self, tool_name: &str, args: &Value) -> bool {
        if tool_name != "browser" {
            return false;
        }
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let has_selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        has_selector && matches!(action, "type" | "click")
    }

    fn extract_bbox_center_from_detection(value: &Value) -> Option<(i64, i64, Value)> {
        let bbox = value
            .get("result")
            .or_else(|| value.get("bounding_box"))
            .or_else(|| {
                value
                    .get("elements")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
            })?;

        let x = Self::value_as_i64(bbox.get("x")?)?;
        let y = Self::value_as_i64(bbox.get("y")?)?;
        let width = bbox
            .get("width")
            .and_then(Self::value_as_i64)
            .unwrap_or(1)
            .max(1);
        let height = bbox
            .get("height")
            .and_then(Self::value_as_i64)
            .unwrap_or(1)
            .max(1);
        let center_x = bbox
            .get("center_x")
            .and_then(Self::value_as_i64)
            .unwrap_or(x + width / 2);
        let center_y = bbox
            .get("center_y")
            .and_then(Self::value_as_i64)
            .unwrap_or(y + height / 2);

        Some((center_x, center_y, bbox.clone()))
    }

    fn value_as_i64(value: &Value) -> Option<i64> {
        value
            .as_i64()
            .or_else(|| value.as_u64().map(|n| n as i64))
            .or_else(|| value.as_f64().map(|n| n.round() as i64))
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    }

    async fn execute_tool_step(
        &self,
        tool_name: &str,
        args: &Value,
        tool_registry: &ToolRegistry,
    ) -> ToolExecutionOutcome {
        let max_attempts = if self.is_selector_based_browser_args(tool_name, args) {
            1
        } else if self.should_retry_tool(tool_name) {
            self.config.max_retries.saturating_add(1)
        } else {
            1
        };
        let mut last_result: Option<Result<Value>> = None;
        let mut last_error = None;
        let mut last_fingerprint = None;
        let mut repeated_failure_count = 0usize;

        for attempt in 0..max_attempts {
            let result = self
                .executor
                .execute(tool_name, args.clone(), tool_registry)
                .await;

            let (failed, error_type, error_message) = match &result {
                Ok(value)
                    if !Self::is_error_response(value)
                        && !self.is_invalid_tool_result(tool_name, value) =>
                {
                    return ToolExecutionOutcome {
                        result,
                        step_result: StepResult {
                            success: true,
                            error: None,
                            retries: attempt as u8,
                        },
                        failure_fingerprint: None,
                    };
                }
                Ok(value) => {
                    let error_type = value.get("error_type").and_then(|v| v.as_str()).unwrap_or(
                        if self.is_invalid_tool_result(tool_name, value) {
                            "invalid_result"
                        } else {
                            "tool_error"
                        },
                    );
                    let error_message = value
                        .get("message")
                        .or_else(|| value.get("error"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(if self.is_invalid_tool_result(tool_name, value) {
                            "tool returned an invalid result"
                        } else {
                            "tool execution failed"
                        })
                        .to_string();
                    (true, error_type.to_string(), error_message)
                }
                Err(err) => {
                    let err_text = err.to_string();
                    let error_type = if err_text.contains("timeout") {
                        "timeout"
                    } else {
                        "executor_error"
                    };
                    (true, error_type.to_string(), err_text)
                }
            };

            if !failed {
                return ToolExecutionOutcome {
                    result,
                    step_result: StepResult {
                        success: true,
                        error: None,
                        retries: attempt as u8,
                    },
                    failure_fingerprint: None,
                };
            }

            let fingerprint =
                self.build_failure_fingerprint(tool_name, &error_type, &error_message);
            if last_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                repeated_failure_count += 1;
            } else {
                repeated_failure_count = 1;
                last_fingerprint = Some(fingerprint.clone());
            }
            last_error = Some(error_message.clone());
            last_result = Some(result);

            if repeated_failure_count >= self.config.max_retries.max(1) {
                break;
            }

            if attempt + 1 < max_attempts {
                let backoff = self
                    .config
                    .retry_backoff_ms
                    .saturating_mul((attempt + 1) as u64);
                tokio::time::sleep(Duration::from_millis(backoff)).await;
            }

            if attempt + 1 < max_attempts && tool_registry.get("screenshot").is_some() {
                let _ = self
                    .executor
                    .execute("screenshot", serde_json::json!({}), tool_registry)
                    .await;
            }
        }

        ToolExecutionOutcome {
            result: last_result.unwrap_or_else(|| {
                Ok(serde_json::json!({
                "status": "error",
                "tool": tool_name,
                "error_type": "unknown",
                "message": "tool execution failed after retries",
                "suggestion": "Capture a fresh screenshot and choose another strategy."
                }))
            }),
            step_result: StepResult {
                success: false,
                error: last_error,
                retries: max_attempts.saturating_sub(1) as u8,
            },
            failure_fingerprint: last_fingerprint,
        }
    }

    fn should_retry_tool(&self, tool_name: &str) -> bool {
        matches!(
            tool_name,
            "browser"
                | "detect_ui_elements"
                | "screen_diff"
                | "mouse"
                | "keyboard"
                | "scroll"
                | "window"
                | "app_state"
                | "wait_for"
        )
    }

    async fn handle_parallel_batch(
        &mut self,
        goal: &str,
        iteration: usize,
        current_plan: &mut String,
        tool_calls: Vec<ToolCall>,
        tool_registry: &ToolRegistry,
        recent_tool_outputs: &mut Vec<String>,
        archived_context: &mut Vec<String>,
        summary_dirty: &mut bool,
        tools_used: &mut Vec<String>,
        options: &PlannerRunOptions,
    ) -> Result<()> {
        if !tool_calls.iter().all(|call| {
            self.executor
                .is_parallel_tool_call_safe(call, tool_registry)
        }) {
            let error_output = "[Recovery] Parallel batches may only contain independent read-only tools. Replan and emit sequential tool calls for mutating actions.".to_string();
            recent_tool_outputs.push(error_output);
            if Self::trim_tool_outputs(recent_tool_outputs, archived_context) {
                *summary_dirty = true;
            }
            return Ok(());
        }

        tracing::info!(count = tool_calls.len(), "executing parallel tool batch");
        let results = self
            .executor
            .execute_parallel_tools(tool_calls.clone(), tool_registry)
            .await;

        let mut batch_failures = Vec::new();
        for ToolResult {
            tool_name,
            arguments,
            output,
            success,
            error,
        } in results
        {
            if success {
                tools_used.push(tool_name.clone());
            } else if let Some(err) = error.clone() {
                batch_failures.push((tool_name.clone(), err));
            }

            self.memory
                .log_tool_execution(
                    &tool_name,
                    &arguments.to_string(),
                    &serde_json::to_string_pretty(&self.sanitize_tool_result(&tool_name, &output))
                        .unwrap_or_else(|_| output.to_string()),
                    success,
                )
                .ok();

            recent_tool_outputs.push(self.format_tool_output_for_prompt(&tool_name, &output));
        }

        if !batch_failures.is_empty() {
            let failure_summary = batch_failures
                .iter()
                .map(|(tool, err)| format!("{}: {}", tool, err))
                .collect::<Vec<_>>()
                .join(" | ");
            *current_plan = self
                .generate_recovery_plan(
                    goal,
                    current_plan,
                    "parallel_batch",
                    &failure_summary,
                    None,
                )
                .await;
            recent_tool_outputs.push(format!(
                "[Recovery]\nParallel batch failed.\nNew plan:\n{}",
                current_plan
            ));
        }

        if Self::trim_tool_outputs(recent_tool_outputs, archived_context) {
            *summary_dirty = true;
        }

        self.emit_progress(
            options,
            if batch_failures.is_empty() {
                "verify"
            } else {
                "recover"
            },
            format!(
                "Processed parallel batch with {} tool calls",
                tool_calls.len()
            ),
            Some(self.build_checkpoint_payload(
                goal,
                current_plan,
                iteration,
                recent_tool_outputs,
                batch_failures.first().map(|(_, err)| err.as_str()),
            )),
        );

        Ok(())
    }

    fn is_invalid_tool_result(&self, tool_name: &str, value: &Value) -> bool {
        if value.is_null() {
            return true;
        }

        match tool_name {
            "search" => value.get("results").is_none() && value.get("total_matches").is_none(),
            "project_map" => value.get("tree").is_none(),
            "filesystem" => {
                // Support standardized filesystem responses:
                // {"status":"ok","data":{"action":"...","path":"..."}}
                let payload = value.get("data").unwrap_or(value);
                payload.get("action").is_none() && payload.get("message").is_none()
            }
            "http" => value.get("status").is_none() || value.get("body").is_none(),
            "git" => value.get("stdout").is_none() && value.get("stderr").is_none(),
            "test_runner" => value.get("exit_code").is_none(),
            "web_search" => value.get("results").is_none(),
            "screenshot" => Self::extract_image_path_from_tool_result(value).is_none(),
            "detect_ui_elements" => {
                value.get("elements").is_none() && !Self::is_error_response(value)
            }
            _ => value
                .as_object()
                .map(|object| object.is_empty())
                .unwrap_or(false),
        }
    }

    fn build_failure_fingerprint(
        &self,
        tool_name: &str,
        error_type: &str,
        error_message: &str,
    ) -> String {
        format!(
            "{}::{}::{}",
            tool_name,
            error_type,
            Self::truncate_text(error_message, 160)
        )
    }

    async fn generate_initial_plan(
        &self,
        goal: &str,
        previous_experience: Option<&ExperienceEntry>,
        tool_registry: &ToolRegistry,
        resume_checkpoint: Option<&str>,
    ) -> Result<String> {
        let mut prompt = format!(
            "Create a concise 3-6 step plan for this goal.\nGoal: {}\nOnly return the plan as numbered steps. Mention when independent read-only tools can be batched in parallel.",
            goal
        );
        if let Some(experience) = previous_experience {
            prompt.push_str(&format!(
                "\nPrevious successful strategy:\n{}",
                Self::truncate_text(&experience.plan, 800)
            ));
        }
        if let Some(checkpoint) = resume_checkpoint {
            prompt.push_str(&format!(
                "\nResume from this checkpoint instead of restarting blindly:\n{}",
                Self::truncate_text(checkpoint, 1000)
            ));
        }

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: Value::String(self.build_system_prompt(
                    "You are planning for Gyro-Claw. Produce a direct actionable plan only.",
                    tool_registry,
                )),
            },
            Message {
                role: "user".to_string(),
                content: Value::String(prompt),
            },
        ];

        let plan = self.llm.chat(&messages).await?;
        tracing::info!(goal = %goal, "planner created initial plan");
        Ok(Self::truncate_text(plan.trim(), 1500))
    }

    async fn generate_recovery_plan(
        &self,
        goal: &str,
        current_plan: &str,
        tool_name: &str,
        error_message: &str,
        error_type: Option<&str>,
    ) -> String {
        let prompt = format!(
            "Goal: {}\nCurrent plan:\n{}\nFailed tool: {}\nFailure type: {}\nFailure message: {}\nProvide an updated numbered plan that avoids repeating the same failure.",
            goal,
            Self::truncate_text(current_plan, 1000),
            tool_name,
            error_type.unwrap_or("unknown"),
            Self::truncate_text(error_message, 600),
        );

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: Value::String(
                    "You are Gyro-Claw recovery planning. Return only an updated numbered plan."
                        .to_string(),
                ),
            },
            Message {
                role: "user".to_string(),
                content: Value::String(prompt),
            },
        ];

        match self.llm.chat(&messages).await {
            Ok(plan) => {
                tracing::info!(tool = %tool_name, error_type = ?error_type, "planner generated recovery plan");
                Self::truncate_text(plan.trim(), 1500)
            }
            Err(err) => {
                tracing::warn!(tool = %tool_name, error = %err, "failed to generate recovery plan");
                format!(
                    "{}\n{}. Recover by avoiding '{}' until new context is gathered.",
                    current_plan, error_message, tool_name
                )
            }
        }
    }

    fn emit_progress(
        &self,
        options: &PlannerRunOptions,
        phase: &str,
        message: String,
        checkpoint: Option<String>,
    ) {
        if let Some(tx) = options.progress_tx.as_ref() {
            let _ = tx.send(PlannerProgressUpdate {
                phase: phase.to_string(),
                message,
                checkpoint,
            });
        }
    }

    fn build_checkpoint_payload(
        &self,
        goal: &str,
        current_plan: &str,
        iteration: usize,
        recent_tool_outputs: &[String],
        last_error: Option<&str>,
    ) -> String {
        serde_json::json!({
            "goal": goal,
            "plan": current_plan,
            "iteration": iteration,
            "recent_tool_outputs": recent_tool_outputs.iter().rev().take(3).cloned().collect::<Vec<_>>(),
            "last_error": last_error,
        })
        .to_string()
    }

    /// Build the system prompt including tool descriptions.
    /// Tool descriptions include name, description, and input schema.
    /// Secret values are NEVER included.
    fn build_system_prompt(&self, full_system: &str, tool_registry: &ToolRegistry) -> String {
        let tools = tool_registry.tool_descriptions();
        let tools_json = serde_json::to_string_pretty(&tools).unwrap_or_default();

        format!("{}\n\nAvailable tools:\n{}", full_system, tools_json)
    }
}
