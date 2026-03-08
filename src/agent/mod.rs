//! # Agent Module
//!
//! Contains the core agent components:
//! - Planner: the agent loop (prompt → tool selection → execution → result)
//! - Executor: secure execution layer with validation and secret injection
//! - Memory: SQLite-backed conversation and task history
//! - Tasks: goal-based active task planning
//! - Test Fix Loop: autonomous self-healing test loop

pub mod executor;
pub mod experience;
pub mod indexer;
pub mod memory;
pub mod planner;
pub mod task_worker;
pub mod tasks;
pub mod test_fix_loop;
pub mod tool_parser;
pub mod worker;
