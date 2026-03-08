//! # Task Planning System
//!
//! Handles advanced goal-based task planning.
//! Allows the agent to construct an explicit plan and execute it step-by-step.

use serde::{Deserialize, Serialize};

/// Status of a multi-step task
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Status of a single step within a task
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

/// A structured step in a planned task
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskStep {
    pub description: String,
    pub tool: Option<String>,
    pub status: StepStatus,
}

/// A comprehensive goal-based task
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Task {
    pub id: String,
    pub goal: String,
    pub steps: Vec<TaskStep>,
    pub status: TaskStatus,
    pub result: Option<String>,
}

impl Task {
    pub fn new(id: String, goal: String, steps: Vec<TaskStep>) -> Self {
        Self {
            id,
            goal,
            steps,
            status: TaskStatus::Pending,
            result: None,
        }
    }

    /// Retrieve the current active step in the task.
    pub fn current_step(&mut self) -> Option<&mut TaskStep> {
        self.steps
            .iter_mut()
            .find(|s| s.status == StepStatus::Pending || s.status == StepStatus::InProgress)
    }

    /// Mark the task as completed with an optional final result summary.
    pub fn complete(&mut self, result: String) {
        self.status = TaskStatus::Completed;
        self.result = Some(result);
        for step in &mut self.steps {
            if step.status == StepStatus::Pending || step.status == StepStatus::InProgress {
                step.status = StepStatus::Skipped;
            }
        }
    }

    /// Generate a readable markdown summary of the task execution.
    pub fn summarize(&self) -> String {
        let mut out = format!(
            "# Task: {}\nStatus: {:?}\n\n## Steps:\n",
            self.goal, self.status
        );
        for (i, step) in self.steps.iter().enumerate() {
            let symbol = match step.status {
                StepStatus::Completed => "✅",
                StepStatus::Failed => "❌",
                StepStatus::InProgress => "▶️",
                StepStatus::Pending => "⏳",
                StepStatus::Skipped => "⏭️",
            };
            let tool_badge = if let Some(t) = &step.tool {
                format!(" `[{}]`", t)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "{}. {} {}{}\n",
                i + 1,
                symbol,
                step.description,
                tool_badge
            ));
        }

        if let Some(res) = &self.result {
            out.push_str(&format!("\n## Final Result:\n{}\n", res));
        }

        out
    }
}
