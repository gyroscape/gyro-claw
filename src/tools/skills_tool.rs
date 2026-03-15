//! # Skills Tool
//!
//! Exposes the skills system to the agent as a callable tool.
//! Actions: `list` (show available skills), `load` (get full instructions for a skill).

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::tools::skills::SkillManager;
use crate::tools::{Tool, ToolSecretPolicy};

/// Tool that lets the agent discover and load skill playbooks.
pub struct SkillsTool {
    manager: Arc<SkillManager>,
}

impl SkillsTool {
    pub fn new(manager: Arc<SkillManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SkillsTool {
    fn name(&self) -> &str {
        "skills"
    }

    fn description(&self) -> &str {
        "Manage reusable skill playbooks. Use action 'list' to see available skills, \
         or action 'load' with a skill name to get its full instructions. \
         Skills provide step-by-step workflows for common tasks like deployment, scaffolding, and more."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "load"],
                    "description": "Action to perform: 'list' shows all available skills, 'load' retrieves full instructions for a specific skill."
                },
                "name": {
                    "type": "string",
                    "description": "The skill name to load (required for 'load' action)."
                }
            },
            "required": ["action"]
        })
    }

    fn is_parallel_safe(&self) -> bool {
        true // read-only operations
    }

    fn secret_policy(&self) -> ToolSecretPolicy {
        ToolSecretPolicy::deny()
    }

    async fn execute(&self, input: Value) -> Result<Value> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "list" => {
                let skills = self.manager.list();
                if skills.is_empty() {
                    return Ok(serde_json::json!({
                        "status": "success",
                        "skills": [],
                        "message": "No skills installed. Create skills at ~/.gyro-claw/skills/<name>/SKILL.md"
                    }));
                }

                let skill_list: Vec<Value> = skills
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name,
                            "description": s.description,
                            "triggers": s.triggers,
                        })
                    })
                    .collect();

                Ok(serde_json::json!({
                    "status": "success",
                    "skills": skill_list,
                    "count": skill_list.len()
                }))
            }
            "load" => {
                let name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter for load action"))?;

                match self.manager.load(name) {
                    Some(content) => Ok(serde_json::json!({
                        "status": "success",
                        "skill_name": name,
                        "instructions": content,
                        "message": "Follow these instructions carefully to complete the task."
                    })),
                    None => Ok(serde_json::json!({
                        "status": "error",
                        "error_type": "skill_not_found",
                        "message": format!("Skill '{}' not found. Use action 'list' to see available skills.", name)
                    })),
                }
            }
            _ => Ok(serde_json::json!({
                "status": "error",
                "error_type": "invalid_action",
                "message": format!("Unknown action '{}'. Use 'list' or 'load'.", action)
            })),
        }
    }
}
