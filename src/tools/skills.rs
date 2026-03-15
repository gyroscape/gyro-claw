//! # Skills System
//!
//! Skills are reusable instruction sets (markdown playbooks) that extend
//! the agent's capabilities for specialized tasks. They are discovered
//! from disk at runtime and loaded into context when needed.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing;

/// A discovered skill entry with metadata parsed from SKILL.md frontmatter.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub path: PathBuf,
}

/// YAML frontmatter parsed from SKILL.md files.
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    triggers: Vec<String>,
}

/// Manages discovery, matching, and loading of skills from disk.
#[derive(Debug, Clone)]
pub struct SkillManager {
    skills: Vec<SkillEntry>,
}

impl SkillManager {
    /// Create an empty SkillManager.
    pub fn new() -> Self {
        Self { skills: Vec::new() }
    }

    /// Discover skills from the standard locations:
    /// 1. `~/.gyro-claw/skills/` (global user skills)
    /// 2. `./workspace/.skills/` (project-local skills, override global)
    pub fn discover() -> Self {
        let mut manager = Self::new();

        // Global skills directory
        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".gyro-claw").join("skills");
            manager.scan_directory(&global_dir);
        }

        // Project-local skills (override global by name)
        let local_dir = PathBuf::from("./workspace/.skills");
        manager.scan_directory(&local_dir);

        tracing::info!(
            "skills discovery complete: {} skill(s) found",
            manager.skills.len()
        );
        manager
    }

    /// Scan a directory for skill folders containing SKILL.md.
    fn scan_directory(&mut self, dir: &Path) {
        if !dir.is_dir() {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("failed to read skills directory {:?}: {}", dir, e);
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_file = path.join("SKILL.md");
            if !skill_file.exists() {
                continue;
            }

            match self.parse_skill(&skill_file) {
                Ok(skill) => {
                    // Project-local skills override global ones with the same name
                    self.skills.retain(|s| s.name != skill.name);
                    tracing::info!("discovered skill: {} at {:?}", skill.name, skill.path);
                    self.skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!("failed to parse skill at {:?}: {}", skill_file, e);
                }
            }
        }
    }

    /// Parse a SKILL.md file, extracting YAML frontmatter and storing the path.
    fn parse_skill(&self, skill_file: &Path) -> anyhow::Result<SkillEntry> {
        let content = std::fs::read_to_string(skill_file)?;

        // Extract YAML frontmatter between --- delimiters
        let frontmatter = Self::extract_frontmatter(&content)
            .ok_or_else(|| anyhow::anyhow!("no YAML frontmatter found in SKILL.md"))?;

        let parsed: SkillFrontmatter = serde_yaml::from_str(&frontmatter)
            .map_err(|e| anyhow::anyhow!("invalid YAML frontmatter: {}", e))?;

        Ok(SkillEntry {
            name: parsed.name,
            description: parsed.description,
            triggers: parsed.triggers,
            path: skill_file.to_path_buf(),
        })
    }

    /// Extract YAML frontmatter from markdown content (between --- delimiters).
    fn extract_frontmatter(content: &str) -> Option<String> {
        let trimmed = content.trim_start();
        if !trimmed.starts_with("---") {
            return None;
        }

        let after_first = &trimmed[3..];
        let end_pos = after_first.find("\n---")?;
        Some(after_first[..end_pos].trim().to_string())
    }

    /// Find skills relevant to the user's query by matching against triggers and description.
    pub fn find_relevant(&self, query: &str) -> Vec<&SkillEntry> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut matches: Vec<(&SkillEntry, usize)> = self
            .skills
            .iter()
            .filter_map(|skill| {
                let mut score = 0usize;

                // Check trigger matches (highest value)
                for trigger in &skill.triggers {
                    let trigger_lower = trigger.to_lowercase();
                    if query_lower.contains(&trigger_lower) {
                        score += 10;
                    }
                }

                // Check name match
                if query_lower.contains(&skill.name.to_lowercase()) {
                    score += 5;
                }

                // Check description word overlap
                let desc_lower = skill.description.to_lowercase();
                for word in &query_words {
                    if word.len() > 2 && desc_lower.contains(*word) {
                        score += 1;
                    }
                }

                if score > 0 {
                    Some((skill, score))
                } else {
                    None
                }
            })
            .collect();

        // Sort by relevance score (highest first)
        matches.sort_by(|a, b| b.1.cmp(&a.1));
        matches.into_iter().map(|(skill, _)| skill).collect()
    }

    /// Load the full content of a skill's SKILL.md file.
    pub fn load(&self, name: &str) -> Option<String> {
        let skill = self.skills.iter().find(|s| s.name == name)?;
        std::fs::read_to_string(&skill.path).ok()
    }

    /// Get all discovered skills.
    pub fn list(&self) -> &[SkillEntry] {
        &self.skills
    }

    /// Generate a summary string for injection into the system prompt.
    pub fn prompt_summary(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut summary = String::from("\n# AVAILABLE SKILLS\n");
        summary.push_str(
            "The following skills are available. Use the `skills` tool with action `load` to load a skill's full instructions before starting the task.\n\n",
        );

        for skill in &self.skills {
            summary.push_str(&format!(
                "- **{}**: {}\n",
                skill.name, skill.description
            ));
        }

        summary
    }
}

impl Default for SkillManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_skill(dir: &Path, name: &str, desc: &str, triggers: &[&str]) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let triggers_yaml: Vec<String> = triggers.iter().map(|t| format!("  - {}", t)).collect();
        let content = format!(
            "---\nname: {}\ndescription: {}\ntriggers:\n{}\n---\n\n## Instructions\n\nDo the thing for {}.\n",
            name, desc, triggers_yaml.join("\n"), name
        );
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn discovers_skills_in_directory() {
        let tmp = TempDir::new().unwrap();
        create_test_skill(tmp.path(), "test-skill", "A test skill", &["test", "demo"]);
        create_test_skill(tmp.path(), "deploy-skill", "Deploy things", &["deploy", "ship"]);

        let mut manager = SkillManager::new();
        manager.scan_directory(tmp.path());

        assert_eq!(manager.list().len(), 2);
    }

    #[test]
    fn find_relevant_matches_triggers() {
        let tmp = TempDir::new().unwrap();
        create_test_skill(tmp.path(), "nextjs-setup", "Scaffold Next.js projects", &["nextjs", "react"]);
        create_test_skill(tmp.path(), "rust-project", "Create Rust projects", &["rust", "cargo"]);

        let mut manager = SkillManager::new();
        manager.scan_directory(tmp.path());

        let matches = manager.find_relevant("create a nextjs app");
        assert!(!matches.is_empty());
        assert_eq!(matches[0].name, "nextjs-setup");
    }

    #[test]
    fn loads_skill_content() {
        let tmp = TempDir::new().unwrap();
        create_test_skill(tmp.path(), "my-skill", "My skill", &["mine"]);

        let mut manager = SkillManager::new();
        manager.scan_directory(tmp.path());

        let content = manager.load("my-skill");
        assert!(content.is_some());
        assert!(content.unwrap().contains("## Instructions"));
    }

    #[test]
    fn project_local_overrides_global() {
        let global_dir = TempDir::new().unwrap();
        let local_dir = TempDir::new().unwrap();
        create_test_skill(global_dir.path(), "shared", "Global version", &["shared"]);
        create_test_skill(local_dir.path(), "shared", "Local version", &["shared"]);

        let mut manager = SkillManager::new();
        manager.scan_directory(global_dir.path());
        manager.scan_directory(local_dir.path());

        assert_eq!(manager.list().len(), 1);
        assert_eq!(manager.list()[0].description, "Local version");
    }

    #[test]
    fn prompt_summary_generates_output() {
        let tmp = TempDir::new().unwrap();
        create_test_skill(tmp.path(), "deploy", "Deploy to production", &["deploy"]);

        let mut manager = SkillManager::new();
        manager.scan_directory(tmp.path());

        let summary = manager.prompt_summary();
        assert!(summary.contains("deploy"));
        assert!(summary.contains("Deploy to production"));
    }

    #[test]
    fn empty_manager_returns_empty_summary() {
        let manager = SkillManager::new();
        assert!(manager.prompt_summary().is_empty());
    }
}
