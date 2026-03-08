//! Experience memory for reusing successful strategies across similar goals.

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::agent::memory::Memory;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceEntry {
    pub id: i64,
    pub goal: String,
    pub plan: String,
    pub tools_used: Vec<String>,
    pub result: String,
    pub timestamp: String,
}

#[derive(Clone)]
pub struct ExperienceStore {
    memory: Memory,
}

impl ExperienceStore {
    pub fn new(memory: Memory) -> Self {
        Self { memory }
    }

    pub fn store_experience(
        &self,
        goal: &str,
        plan: &str,
        tools: &[String],
        result: &str,
    ) -> Result<()> {
        store_experience(&self.memory, goal, plan, tools, result)
    }

    pub fn find_similar_experience(&self, goal: &str) -> Result<Option<ExperienceEntry>> {
        find_similar_experience(&self.memory, goal)
    }
}

pub fn store_experience(
    memory: &Memory,
    goal: &str,
    plan: &str,
    tools: &[String],
    result: &str,
) -> Result<()> {
    let tool_list = serde_json::to_string(tools).context("Failed to serialize tools_used")?;
    let conn = memory
        .conn
        .lock()
        .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
    conn.execute(
        "INSERT INTO experience_log (goal, plan, tools_used, result) VALUES (?1, ?2, ?3, ?4)",
        params![goal, plan, tool_list, result],
    )
    .context("Failed to store experience log entry")?;
    Ok(())
}

pub fn find_similar_experience(memory: &Memory, goal: &str) -> Result<Option<ExperienceEntry>> {
    let conn = memory
        .conn
        .lock()
        .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, goal, plan, tools_used, result, datetime(timestamp, 'localtime')
             FROM experience_log
             ORDER BY id DESC
             LIMIT 100",
        )
        .context("Failed to prepare experience query")?;

    let rows = stmt
        .query_map([], |row| {
            let tools_json: String = row.get(3)?;
            let tools_used =
                serde_json::from_str::<Vec<String>>(&tools_json).unwrap_or_else(|_| Vec::new());
            Ok(ExperienceEntry {
                id: row.get(0)?,
                goal: row.get(1)?,
                plan: row.get(2)?,
                tools_used,
                result: row.get(4)?,
                timestamp: row.get(5)?,
            })
        })
        .context("Failed to load experiences")?;

    let mut best_match: Option<(usize, ExperienceEntry)> = None;
    let target_keywords = keywords(goal);

    for row in rows {
        let entry = row?;
        let score = similarity_score(&target_keywords, &entry.goal);
        if score == 0 {
            continue;
        }

        match &best_match {
            Some((best_score, _)) if *best_score >= score => {}
            _ => best_match = Some((score, entry)),
        }
    }

    Ok(best_match.map(|(_, entry)| entry))
}

fn keywords(goal: &str) -> HashSet<String> {
    goal.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() >= 4)
        .map(|word| word.to_string())
        .collect()
}

fn similarity_score(target_keywords: &HashSet<String>, candidate_goal: &str) -> usize {
    if target_keywords.is_empty() {
        return 0;
    }

    let candidate_keywords = keywords(candidate_goal);
    let overlap = target_keywords.intersection(&candidate_keywords).count();
    let substring_bonus = usize::from(
        candidate_goal
            .to_lowercase()
            .contains(&target_keywords.iter().next().cloned().unwrap_or_default()),
    );

    overlap * 2 + substring_bonus
}

#[cfg(test)]
mod tests {
    use super::similarity_score;
    use std::collections::HashSet;

    #[test]
    fn scores_keyword_overlap() {
        let mut target = HashSet::new();
        target.insert("planner".to_string());
        target.insert("retry".to_string());
        assert!(similarity_score(&target, "add planner retry handling") > 0);
        assert_eq!(similarity_score(&target, "unrelated weather report"), 0);
    }
}
