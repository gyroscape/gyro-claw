//! # Task State
//!
//! Represents a multi-step task for workflow automation and multi-step reasoning.
//! Tasks can be broken into steps, tracked for progress, and stored in memory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Status of a task or step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// A single step within a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    /// Unique step ID
    pub id: String,
    /// Description of what this step does
    pub description: String,
    /// Tool to use (if any)
    pub tool_name: Option<String>,
    /// Tool arguments (if any)
    pub tool_arguments: Option<serde_json::Value>,
    /// Step status
    pub status: TaskStatus,
    /// Step result / output
    pub result: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
}

impl TaskStep {
    pub fn new(description: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.to_string(),
            tool_name: None,
            tool_arguments: None,
            status: TaskStatus::Pending,
            result: None,
            error: None,
        }
    }

    /// Create a step that uses a specific tool.
    pub fn with_tool(description: &str, tool_name: &str, arguments: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.to_string(),
            tool_name: Some(tool_name.to_string()),
            tool_arguments: Some(arguments),
            status: TaskStatus::Pending,
            result: None,
            error: None,
        }
    }
}

/// A multi-step task that enables workflow automation and complex reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task ID
    pub id: String,
    /// High-level goal description
    pub goal: String,
    /// Ordered list of steps to accomplish the goal
    pub steps: Vec<TaskStep>,
    /// Overall task status
    pub status: TaskStatus,
    /// Final result summary
    pub result: Option<String>,
    /// When the task was created
    pub created_at: DateTime<Utc>,
    /// When the task was last updated
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// Create a new task with a goal and no steps yet.
    pub fn new(goal: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            goal: goal.to_string(),
            steps: Vec::new(),
            status: TaskStatus::Pending,
            result: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Add a step to the task.
    pub fn add_step(&mut self, step: TaskStep) {
        self.steps.push(step);
        self.updated_at = Utc::now();
    }

    /// Mark the task as in-progress.
    pub fn start(&mut self) {
        self.status = TaskStatus::InProgress;
        self.updated_at = Utc::now();
    }

    /// Mark a step as completed with a result.
    pub fn complete_step(&mut self, step_id: &str, result: &str) {
        if let Some(step) = self.steps.iter_mut().find(|s| s.id == step_id) {
            step.status = TaskStatus::Completed;
            step.result = Some(result.to_string());
        }
        self.updated_at = Utc::now();
    }

    /// Mark a step as failed with an error.
    pub fn fail_step(&mut self, step_id: &str, error: &str) {
        if let Some(step) = self.steps.iter_mut().find(|s| s.id == step_id) {
            step.status = TaskStatus::Failed;
            step.error = Some(error.to_string());
        }
        self.updated_at = Utc::now();
    }

    /// Mark the entire task as completed.
    pub fn complete(&mut self, result: &str) {
        self.status = TaskStatus::Completed;
        self.result = Some(result.to_string());
        self.updated_at = Utc::now();
    }

    /// Mark the entire task as failed.
    pub fn fail(&mut self, error: &str) {
        self.status = TaskStatus::Failed;
        self.result = Some(error.to_string());
        self.updated_at = Utc::now();
    }

    /// Get the next pending step, if any.
    pub fn next_pending_step(&self) -> Option<&TaskStep> {
        self.steps.iter().find(|s| s.status == TaskStatus::Pending)
    }

    /// Check if all steps are completed.
    pub fn all_steps_completed(&self) -> bool {
        self.steps.iter().all(|s| s.status == TaskStatus::Completed)
    }

    /// Get a summary of the task progress.
    pub fn progress_summary(&self) -> String {
        let total = self.steps.len();
        let completed = self
            .steps
            .iter()
            .filter(|s| s.status == TaskStatus::Completed)
            .count();
        let failed = self
            .steps
            .iter()
            .filter(|s| s.status == TaskStatus::Failed)
            .count();

        format!(
            "Task '{}': {}/{} steps completed, {} failed (status: {})",
            self.goal, completed, total, failed, self.status
        )
    }
}
