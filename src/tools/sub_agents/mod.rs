use async_trait::async_trait;

pub mod browser;
pub mod coder;
pub mod researcher;

/// The role that a sub-agent should play, which dictates the strict subset of tools it has access to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentRole {
    Researcher,
    Coder,
    Browser,
}

/// A factory capable of securely instantiating isolated sub-agents.
#[async_trait]
pub trait SubAgentFactory: Send + Sync {
    /// Run a designated role-based sub-agent to fulfill a single instruction.
    /// This method configures an isolated Planner and ToolRegistry based exactly
    /// on the permitted role subset.
    async fn run_sub_agent(&self, role: SubAgentRole, instruction: &str) -> std::result::Result<String, String>;
}
