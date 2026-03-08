//! # Memory Module
//!
//! SQLite-backed storage for conversation history, tool execution logs, reusable
//! experience, and resumable long-running tasks.

use crate::config::MemoryConfig;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const HARD_MAX_LOGS: i64 = 5000;
const HARD_MAX_EVENTS: i64 = 1000;
const HARD_MAX_SCREENSHOTS: i64 = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: i64,
    pub goal: String,
    pub status: String,
    pub progress: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub checkpoint: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// SQLite-backed persistent memory.
#[derive(Clone)]
pub struct Memory {
    pub conn: Arc<Mutex<Connection>>,
}

impl Memory {
    /// Open or create a SQLite database at the given path.
    /// Auto-creates the required tables if they don't exist.
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open SQLite database: {}", db_path))?;

        // SECURITY FIX: Enable WAL (Write-Ahead Logging) mode for non-blocking reads
        // and improved concurrency. Critical for the API server where reads should not
        // block during writes.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .context("Failed to set SQLite WAL mode")?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS conversations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                role        TEXT    NOT NULL,
                content     TEXT    NOT NULL,
                created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                goal        TEXT    NOT NULL,
                status      TEXT    NOT NULL DEFAULT 'queued',
                result      TEXT,
                created_at  DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at  DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS tool_logs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                tool_name   TEXT    NOT NULL,
                input       TEXT    NOT NULL,
                output      TEXT    NOT NULL,
                success     BOOLEAN NOT NULL DEFAULT 1,
                prev_hash   TEXT,
                log_hash    TEXT,
                created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS project_facts (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                topic       TEXT    NOT NULL,
                fact        TEXT    NOT NULL,
                created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS semantic_index (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path   TEXT    NOT NULL,
                chunk_text  TEXT    NOT NULL,
                embedding   TEXT    NOT NULL,
                updated_at  DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS experience_log (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                goal        TEXT    NOT NULL,
                plan        TEXT    NOT NULL,
                tools_used  TEXT    NOT NULL,
                result      TEXT    NOT NULL,
                timestamp   DATETIME DEFAULT CURRENT_TIMESTAMP
            );
            ",
        )
        .context("Failed to create memory tables")?;

        Self::migrate_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn migrate_schema(conn: &Connection) -> Result<()> {
        Self::ensure_column(conn, "tasks", "progress", "TEXT")?;
        Self::ensure_column(conn, "tasks", "checkpoint", "TEXT")?;
        Self::ensure_column(conn, "tasks", "error", "TEXT")?;
        Self::ensure_column(conn, "tool_logs", "prev_hash", "TEXT")?;
        Self::ensure_column(conn, "tool_logs", "log_hash", "TEXT")?;
        Ok(())
    }

    fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
        let pragma = format!("PRAGMA table_info({table})");
        let mut stmt = conn
            .prepare(&pragma)
            .with_context(|| format!("Failed to inspect schema for table {}", table))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let existing: String = row.get(1)?;
            if existing == column {
                return Ok(());
            }
        }

        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to add column {}.{}", table, column))?;
        Ok(())
    }

    /// Prune database tables to enforce growth limits.
    pub fn prune_memory(&self, config: &MemoryConfig) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;

        let tx = conn
            .unchecked_transaction()
            .context("Failed to begin transaction for pruning")?;

        tx.execute(
            "DELETE FROM project_facts WHERE id < (
                SELECT id FROM project_facts ORDER BY id DESC LIMIT 1 OFFSET ?1
            )",
            params![config.max_project_facts],
        )
        .context("Failed to prune project_facts")?;

        tx.execute(
            "DELETE FROM tool_logs WHERE id < (
                SELECT id FROM tool_logs ORDER BY id DESC LIMIT 1 OFFSET ?1
            )",
            params![config.max_logs],
        )
        .context("Failed to prune tool_logs")?;

        tx.execute(
            "DELETE FROM conversations WHERE id < (
                SELECT id FROM conversations ORDER BY id DESC LIMIT 1 OFFSET ?1
            )",
            params![config.max_events],
        )
        .context("Failed to prune conversations")?;

        tx.execute(
            "DELETE FROM tool_logs
             WHERE tool_name = 'screenshot'
               AND id NOT IN (
                    SELECT id
                    FROM tool_logs
                    WHERE tool_name = 'screenshot'
                    ORDER BY id DESC
                    LIMIT ?1
               )",
            params![config.max_screenshots],
        )
        .context("Failed to prune screenshot tool logs")?;

        tx.execute(
            "DELETE FROM semantic_index WHERE id < (
                SELECT id FROM semantic_index ORDER BY id DESC LIMIT 1 OFFSET ?1
            )",
            params![config.max_semantic_chunks],
        )
        .context("Failed to prune semantic_index")?;

        tx.commit()
            .context("Failed to commit pruning transaction")?;
        Ok(())
    }

    /// Create an in-memory database (useful for testing or ephemeral sessions).
    pub fn in_memory() -> Result<Self> {
        Self::new(":memory:")
    }

    /// Add a conversation entry (user, assistant, system, or tool message).
    pub fn add_conversation_entry(&self, role: &str, content: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO conversations (role, content) VALUES (?1, ?2)",
            params![role, content],
        )
        .context("Failed to insert conversation entry")?;
        Self::prune_table_limit(&conn, "conversations", HARD_MAX_EVENTS)
            .context("Failed to auto-prune conversation events")?;
        Ok(())
    }

    /// Retrieve the last N conversation entries.
    pub fn get_recent_conversations(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT role, content FROM conversations ORDER BY id DESC LIMIT ?1")
            .context("Failed to prepare conversation query")?;

        let rows = stmt
            .query_map(params![limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("Failed to query conversations")?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        results.reverse();
        Ok(results)
    }

    /// Log a tool execution with input, output, and success status.
    pub fn log_tool_execution(
        &self,
        tool_name: &str,
        input: &str,
        output: &str,
        success: bool,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        let created_at = Utc::now().to_rfc3339();
        let prev_hash = conn
            .query_row(
                "SELECT log_hash
                 FROM tool_logs
                 WHERE log_hash IS NOT NULL AND log_hash != ''
                 ORDER BY id DESC
                 LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| "GENESIS".to_string());

        let log_hash =
            Self::compute_log_hash(&prev_hash, tool_name, input, output, success, &created_at);

        conn.execute(
            "INSERT INTO tool_logs (tool_name, input, output, success, prev_hash, log_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![tool_name, input, output, success, prev_hash, log_hash, created_at],
        )
        .context("Failed to log tool execution")?;

        Self::prune_table_limit(&conn, "tool_logs", HARD_MAX_LOGS)
            .context("Failed to auto-prune tool logs")?;
        Self::prune_screenshot_logs(&conn, HARD_MAX_SCREENSHOTS)
            .context("Failed to auto-prune screenshot logs")?;
        Ok(())
    }

    pub fn verify_tool_log_chain(&self) -> Result<(bool, String)> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, tool_name, input, output, success, created_at, prev_hash, log_hash
                 FROM tool_logs
                 WHERE log_hash IS NOT NULL AND log_hash != ''
                 ORDER BY id ASC",
            )
            .context("Failed to prepare log chain verification query")?;

        let mut rows = stmt.query([])?;
        let mut expected_prev = "GENESIS".to_string();
        let mut verified = 0usize;

        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let tool_name: String = row.get(1)?;
            let input: String = row.get(2)?;
            let output: String = row.get(3)?;
            let success: bool = row.get(4)?;
            let created_at: String = row.get(5)?;
            let prev_hash: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();
            let log_hash: String = row.get::<_, Option<String>>(7)?.unwrap_or_default();

            if prev_hash != expected_prev {
                return Ok((
                    false,
                    format!("Log chain mismatch at entry {} (prev hash mismatch)", id),
                ));
            }

            let computed = Self::compute_log_hash(
                &prev_hash,
                &tool_name,
                &input,
                &output,
                success,
                &created_at,
            );
            if computed != log_hash {
                return Ok((
                    false,
                    format!("Log chain mismatch at entry {} (hash mismatch)", id),
                ));
            }

            expected_prev = log_hash;
            verified += 1;
        }

        Ok((
            true,
            format!("Verified {} hash-chained log entries", verified),
        ))
    }

    /// Retrieve recent tool activity logs.
    pub fn get_tool_logs(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String, String, bool, String)>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT tool_name, input, output, success, datetime(created_at, 'localtime') FROM tool_logs ORDER BY id DESC LIMIT ?1")
            .context("Failed to prepare tool log statement")?;

        let rows = stmt
            .query_map(params![limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .context("Failed to query tool logs")?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Create a new persisted task and return its ID.
    pub fn create_task(&self, goal: &str) -> Result<i64> {
        self.enqueue_task(goal)
    }

    /// Update task status and optionally the result.
    pub fn update_task(&self, task_id: i64, status: &str, result: Option<&str>) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE tasks
             SET status = ?1,
                 result = COALESCE(?2, result),
                 progress = COALESCE(progress, ?1),
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?3",
            params![status, result, task_id],
        )
        .context("Failed to update task")?;
        Ok(())
    }

    /// Store a persistent fact about the project.
    pub fn store_fact(&self, topic: &str, fact: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO project_facts (topic, fact) VALUES (?1, ?2)",
            params![topic, fact],
        )
        .context("Failed to store project fact")?;
        Ok(())
    }

    /// Retrieve all stored facts formatted as a block for the system prompt.
    pub fn get_all_facts_text(&self) -> Result<String> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT topic, fact FROM project_facts ORDER BY id ASC LIMIT 50")
            .context("Failed to prepare project_facts query")?;

        let facts_iter = stmt
            .query_map([], |row| {
                let topic: String = row.get(0)?;
                let fact: String = row.get(1)?;
                Ok(format!("- [{}]: {}", topic, fact))
            })
            .context("Failed to query project_facts")?;

        let mut all_facts = Vec::new();
        for fact in facts_iter.flatten() {
            all_facts.push(fact);
        }

        if all_facts.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(
                "\n# PROJECT KNOWLEDGE BASE:\n{}\n",
                all_facts.join("\n")
            ))
        }
    }

    /// Add a long-running task to the resumable queue.
    pub fn enqueue_task(&self, goal: &str) -> Result<i64> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO tasks (goal, status, progress) VALUES (?1, 'queued', 'queued')",
            params![goal],
        )
        .context("Failed to enqueue task")?;
        Ok(conn.last_insert_rowid())
    }

    /// Claim the next queued or stale-running task for processing.
    pub fn claim_next_task(&self, stale_after_secs: i64) -> Result<Option<TaskRecord>> {
        let conn = self.lock_conn()?;
        let tx = conn
            .unchecked_transaction()
            .context("Failed to begin task claim transaction")?;
        let stale_after = format!("-{} seconds", stale_after_secs);

        let task = tx
            .query_row(
                "SELECT id,
                        goal,
                        status,
                        progress,
                        result,
                        error,
                        checkpoint,
                        datetime(created_at, 'localtime'),
                        datetime(updated_at, 'localtime')
                 FROM tasks
                 WHERE status = 'queued'
                    OR (status = 'running' AND updated_at < datetime('now', ?1))
                 ORDER BY CASE status WHEN 'running' THEN 0 ELSE 1 END, id ASC
                 LIMIT 1",
                params![stale_after],
                Self::row_to_task_record,
            )
            .optional()
            .context("Failed to load next task")?;

        if let Some(task) = task {
            let progress = if task.checkpoint.is_some() {
                "resuming from checkpoint"
            } else {
                "running"
            };
            tx.execute(
                "UPDATE tasks
                 SET status = 'running',
                     progress = ?2,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![task.id, progress],
            )
            .context("Failed to claim task")?;
            tx.commit().context("Failed to commit claimed task")?;
            return self.get_task(task.id);
        }

        tx.commit().ok();
        Ok(None)
    }

    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id,
                        goal,
                        status,
                        progress,
                        result,
                        error,
                        checkpoint,
                        datetime(created_at, 'localtime'),
                        datetime(updated_at, 'localtime')
                 FROM tasks
                 ORDER BY id DESC
                 LIMIT 100",
            )
            .context("Failed to prepare task list query")?;

        let rows = stmt
            .query_map([], Self::row_to_task_record)
            .context("Failed to query tasks")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    pub fn get_task(&self, id: i64) -> Result<Option<TaskRecord>> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id,
                    goal,
                    status,
                    progress,
                    result,
                    error,
                    checkpoint,
                    datetime(created_at, 'localtime'),
                    datetime(updated_at, 'localtime')
             FROM tasks
             WHERE id = ?1",
            params![id],
            Self::row_to_task_record,
        )
        .optional()
        .context("Failed to fetch task")
    }

    pub fn touch_task(&self, id: i64) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE tasks SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status = 'running'",
            params![id],
        )
        .context("Failed to touch task")?;
        Ok(())
    }

    pub fn update_task_progress(
        &self,
        id: i64,
        progress: &str,
        checkpoint: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE tasks
             SET progress = ?2,
                 checkpoint = COALESCE(?3, checkpoint),
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1 AND status = 'running'",
            params![id, progress, checkpoint],
        )
        .context("Failed to update task progress")?;
        Ok(())
    }

    pub fn complete_task(&self, id: i64, result: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE tasks
             SET status = 'completed',
                 progress = 'completed',
                 result = ?2,
                 error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![id, result],
        )
        .context("Failed to mark task completed")?;
        Ok(())
    }

    pub fn fail_task(&self, id: i64, error: &str, checkpoint: Option<&str>) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE tasks
             SET status = 'failed',
                 progress = COALESCE(progress, 'failed'),
                 error = ?2,
                 checkpoint = COALESCE(?3, checkpoint),
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![id, error, checkpoint],
        )
        .context("Failed to mark task failed")?;
        Ok(())
    }

    pub fn cancel_task(&self, id: i64) -> Result<bool> {
        let conn = self.lock_conn()?;
        let affected = conn
            .execute(
                "UPDATE tasks
             SET status = 'failed',
                 progress = 'cancelled by user',
                 error = 'cancelled by user',
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1 AND status IN ('queued', 'running')",
                params![id],
            )
            .context("Failed to cancel task")?;
        Ok(affected > 0)
    }

    pub fn is_task_cancelled(&self, id: i64) -> Result<bool> {
        let conn = self.lock_conn()?;
        let state = conn
            .query_row(
                "SELECT status, COALESCE(progress, ''), COALESCE(error, '')
                 FROM tasks WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .context("Failed to inspect task cancellation state")?;

        Ok(matches!(
            state,
            Some((status, progress, error))
                if status == "failed"
                    && (progress.contains("cancelled") || error.contains("cancelled"))
        ))
    }

    /// Requeue stale running tasks so the worker can resume them from checkpoint.
    pub fn recover_stuck_tasks(&self) -> Result<usize> {
        let conn = self.lock_conn()?;
        let affected = conn
            .execute(
                "UPDATE tasks
                 SET status = 'queued',
                     progress = CASE
                         WHEN checkpoint IS NOT NULL THEN 'resuming from checkpoint'
                         ELSE 'recovered after interruption'
                     END,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE status = 'running'
                   AND updated_at < datetime('now', '-60 seconds')",
                [],
            )
            .context("Failed to recover stuck tasks")?;
        Ok(affected)
    }

    // --- Legacy background task wrappers ---

    pub fn queue_background_task(&self, goal: &str) -> Result<i64> {
        self.enqueue_task(goal)
    }

    pub fn claim_next_background_task(&self) -> Result<Option<(i64, String)>> {
        Ok(self.claim_next_task(60)?.map(|task| (task.id, task.goal)))
    }

    pub fn finish_background_task(
        &self,
        id: i64,
        status: &str,
        result: Option<&str>,
    ) -> Result<()> {
        match status {
            "completed" => self.complete_task(id, result.unwrap_or("completed")),
            "failed" | "cancelled" => {
                self.fail_task(id, result.unwrap_or("background task failed"), None)
            }
            other => self.update_task(id, other, result),
        }
    }

    pub fn cancel_background_task(&self, id: i64) -> Result<bool> {
        self.cancel_task(id)
    }

    pub fn list_background_tasks(&self) -> Result<Vec<(i64, String, String, String)>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .map(|task| (task.id, task.goal, task.status, task.created_at))
            .collect())
    }

    pub fn update_heartbeat(&self, id: i64) -> Result<()> {
        self.touch_task(id)
    }

    fn row_to_task_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
        Ok(TaskRecord {
            id: row.get(0)?,
            goal: row.get(1)?,
            status: row.get(2)?,
            progress: row.get(3)?,
            result: row.get(4)?,
            error: row.get(5)?,
            checkpoint: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))
    }

    fn prune_table_limit(conn: &Connection, table: &str, limit: i64) -> Result<()> {
        let sql = format!(
            "DELETE FROM {table} WHERE id < (
                SELECT id FROM {table} ORDER BY id DESC LIMIT 1 OFFSET ?1
            )"
        );
        conn.execute(&sql, params![limit])
            .with_context(|| format!("Failed pruning table {}", table))?;
        Ok(())
    }

    fn prune_screenshot_logs(conn: &Connection, limit: i64) -> Result<()> {
        conn.execute(
            "DELETE FROM tool_logs
             WHERE tool_name = 'screenshot'
               AND id NOT IN (
                    SELECT id
                    FROM tool_logs
                    WHERE tool_name = 'screenshot'
                    ORDER BY id DESC
                    LIMIT ?1
               )",
            params![limit],
        )
        .context("Failed pruning screenshot logs")?;
        Ok(())
    }

    fn compute_log_hash(
        prev_hash: &str,
        tool_name: &str,
        input: &str,
        output: &str,
        success: bool,
        created_at: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(tool_name.as_bytes());
        hasher.update(input.as_bytes());
        hasher.update(output.as_bytes());
        hasher.update(if success { b"1" } else { b"0" });
        hasher.update(created_at.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}
