//! # Dedicated Test-Fix Loop
//!
//! An autonomous debugging loop specifically designed to parse test output,
//! locate failing code, and apply fixes repeatedly until success or max attempts.

use anyhow::Result;
use console::{style, Term};

use crate::agent::planner::Planner;
use crate::tools::ToolRegistry;

pub struct TestFixLoop<'a> {
    planner: &'a mut Planner,
    registry: &'a ToolRegistry,
    max_fix_attempts: usize,
    test_name: Option<String>,
}

impl<'a> TestFixLoop<'a> {
    pub fn new(
        planner: &'a mut Planner,
        registry: &'a ToolRegistry,
        test_name: Option<String>,
    ) -> Self {
        Self {
            planner,
            registry,
            max_fix_attempts: 5,
            test_name,
        }
    }

    /// Set the maximum number of times to run the test-parse-fix-retest loop.
    pub fn set_max_attempts(&mut self, attempts: usize) {
        self.max_fix_attempts = attempts;
    }

    /// Run the fully autonomous test-fix loop.
    pub async fn run(&mut self) -> Result<String> {
        let term = Term::stderr();
        term.write_line("\n🚀 Starting Autonomous Test-Fix Loop...")
            .ok();

        let mut prompt = String::from("You are now in the Autonomous Test-Fix Loop. \
            Your goal is strictly: run tests -> parse output -> locate failing code -> edit code -> run tests again. \
            Do not stop until `cargo test` exits with success.");

        if let Some(name) = &self.test_name {
            prompt.push_str(&format!(" Specifically, fix the test named '{}'.", name));
        }

        // We delegate to the planner but give it a massively reinforced objective.
        // We override iterations so it doesn't give up before the max_fix_attempts limit.
        self.planner.set_limits(self.max_fix_attempts * 4, 600);

        let result = self.planner.run(&prompt, self.registry).await?;

        term.write_line(&format!(
            "\n✅ {}",
            style("Auto-Fix Session Completed!").bold().green()
        ))
        .ok();
        Ok(result)
    }
}
