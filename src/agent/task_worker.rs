//! Resumable background task worker for long-running autonomous jobs.

use anyhow::Result;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

use crate::agent::memory::{Memory, TaskRecord};
use crate::agent::planner::{Planner, PlannerProgressUpdate, PlannerRunOptions};
use crate::config::Config;
use crate::tools::ToolRegistry;

/// Factory for creating fresh agent instances per task.
pub trait AgentFactory: Send + Sync {
    fn create_agent(&self) -> Result<(Planner, ToolRegistry)>;
}

pub struct TaskWorker {
    memory: Memory,
    factory: Box<dyn AgentFactory>,
    config: Config,
    poll_interval: Duration,
    stale_after: Duration,
}

impl TaskWorker {
    pub fn new(memory: Memory, config: Config, factory: Box<dyn AgentFactory>) -> Self {
        Self {
            memory,
            factory,
            config,
            poll_interval: Duration::from_secs(3),
            stale_after: Duration::from_secs(60),
        }
    }

    pub async fn run_loop(&mut self) -> Result<()> {
        tracing::info!("task worker started");

        if let Ok(recovered_count) = self.memory.recover_stuck_tasks() {
            if recovered_count > 0 {
                tracing::warn!(recovered_count, "recovered interrupted tasks");
            }
        }

        loop {
            match self
                .memory
                .claim_next_task(self.stale_after.as_secs() as i64)
            {
                Ok(Some(task)) => {
                    if let Err(err) = self.execute_task(task).await {
                        tracing::error!(error = %err, "task worker execution failed");
                    }
                }
                Ok(None) => sleep(self.poll_interval).await,
                Err(err) => {
                    tracing::warn!(error = %err, "task worker poll failed");
                    sleep(self.poll_interval).await;
                }
            }
        }
    }

    async fn execute_task(&self, task: TaskRecord) -> Result<()> {
        tracing::info!(task_id = task.id, goal = %task.goal, "starting background task");

        let (mut planner, registry) = self.factory.create_agent()?;
        planner.set_limits(self.config.max_iterations.max(50), 600);

        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<PlannerProgressUpdate>();
        let memory = self.memory.clone();
        let task_id = task.id;
        let progress_handle = tokio::spawn(async move {
            while let Some(update) = progress_rx.recv().await {
                memory
                    .update_task_progress(task_id, &update.message, update.checkpoint.as_deref())
                    .ok();
            }
        });

        let heartbeat_memory = self.memory.clone();
        let heartbeat_task_id = task.id;
        let heartbeat_handle = tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(10)).await;
                heartbeat_memory.touch_task(heartbeat_task_id).ok();
            }
        });

        let mut goal = task.goal.clone();
        if let Some(checkpoint) = task.checkpoint.as_deref() {
            goal = format!(
                "Resume the interrupted task.\nOriginal goal: {}\nCheckpoint:\n{}",
                task.goal, checkpoint
            );
        }

        let mut planner_future = Pin::from(Box::new(planner.run_with_options(
            &goal,
            &registry,
            PlannerRunOptions {
                task_id: Some(task.id),
                resume_checkpoint: task.checkpoint.clone(),
                progress_tx: Some(progress_tx),
            },
        )));

        let result = loop {
            tokio::select! {
                planner_result = &mut planner_future => {
                    break planner_result;
                }
                _ = sleep(Duration::from_secs(2)) => {
                    if self.memory.is_task_cancelled(task.id)? {
                        tracing::warn!(task_id = task.id, "cancelling running task");
                        break Ok(String::from("Task cancelled by user."));
                    }
                }
            }
        };

        heartbeat_handle.abort();
        progress_handle.abort();

        match result {
            Ok(response) if self.memory.is_task_cancelled(task.id)? => {
                self.memory
                    .fail_task(task.id, "cancelled by user", task.checkpoint.as_deref())?;
                tracing::warn!(task_id = task.id, "task cancelled");
            }
            Ok(response) => {
                self.memory.complete_task(task.id, &response)?;
                tracing::info!(task_id = task.id, "task completed");
            }
            Err(err) => {
                let checkpoint = self
                    .memory
                    .get_task(task.id)?
                    .and_then(|current| current.checkpoint);
                self.memory
                    .fail_task(task.id, &err.to_string(), checkpoint.as_deref())?;
                tracing::error!(task_id = task.id, error = %err, "task failed");
            }
        }

        Ok(())
    }
}

pub async fn task_worker(worker: &mut TaskWorker) -> Result<()> {
    worker.run_loop().await
}
