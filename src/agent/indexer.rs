//! # File Indexer / Semantic Search Engine
//!
//! Handles scanning the repository, chunking files, computing embeddings
//! via the LLM API, storing them in SQLite, and executing cosine similarity
//! searches across the chunks.

use anyhow::Result;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

use crate::agent::memory::Memory;
use crate::llm::client::LlmClient;

pub struct SemanticIndexer {
    memory: Memory,
    llm: LlmClient,
}

impl SemanticIndexer {
    pub fn new(memory: Memory, llm: LlmClient) -> Self {
        Self { memory, llm }
    }

    /// Delete existing index data and re-index the given directory.
    pub async fn reindex_all(&self, dir: &Path) -> Result<usize> {
        let conn = self
            .memory
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        conn.execute("DELETE FROM semantic_index", [])?;
        drop(conn);

        let mut chunks_added = 0;
        let mut files_to_index = Vec::new();

        for entry in WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path().to_path_buf();
            // Skip target, .git, and common binary extensions
            if path.to_string_lossy().contains("/target/")
                || path.to_string_lossy().contains("/.git/")
                || path.extension().and_then(|s| s.to_str()) == Some("png")
                || path.extension().and_then(|s| s.to_str()) == Some("jpg")
            {
                continue;
            }
            files_to_index.push(path);
        }

        let term = console::Term::stderr();
        term.write_line(&format!("Indexing {} files...", files_to_index.len()))
            .ok();

        for path in files_to_index {
            if let Ok(content) = fs::read_to_string(&path) {
                // VERY naive chunking for demonstration: slice by ~1000 characters cleanly on lines
                let chunks = self.chunk_text(&content, 1000);
                for chunk in chunks {
                    if chunk.trim().is_empty() {
                        continue;
                    }
                    if let Ok(embedding) = self.llm.get_embedding(&chunk).await {
                        chunks_added += 1;
                        self.save_chunk(&path, &chunk, embedding)?;
                    }
                }
            }
        }

        term.write_line(&format!(
            "Indexing complete! Added {} semantic chunks.",
            chunks_added
        ))
        .ok();
        Ok(chunks_added)
    }

    /// Store a single embedded chunk to SQLite.
    fn save_chunk(&self, path: &Path, text: &str, embedding: Vec<f32>) -> Result<()> {
        let embedding_json = serde_json::to_string(&embedding)?;
        let path_str = path.to_string_lossy().to_string();

        let conn = self
            .memory
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        conn.execute(
            "INSERT INTO semantic_index (file_path, chunk_text, embedding) VALUES (?1, ?2, ?3)",
            rusqlite::params![path_str, text, embedding_json],
        )?;
        Ok(())
    }

    /// Perform a Cosine Similarity search over the stored vectors.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, String, f32)>> {
        // 1. Get embedding for the query string
        let query_embed = self.llm.get_embedding(query).await?;

        // 2. Fetch all stored embeddings (Brute force calculation in Rust)
        // For sub-10k chunk scale, Rust will do this math in milliseconds.
        let conn = self
            .memory
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        let mut stmt =
            conn.prepare("SELECT file_path, chunk_text, embedding FROM semantic_index")?;

        let mut results: Vec<(String, String, f32)> = Vec::new();

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        for (path, text, embed_str) in rows.flatten() {
            if let Ok(stored_embed) = serde_json::from_str::<Vec<f32>>(&embed_str) {
                let score = cosine_similarity(&query_embed, &stored_embed);
                results.push((path, text, score));
            }
        }

        // 3. Sort by descending similarity score
        results.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results)
    }

    /// Naive splitting function to divide large files into semi-readable chunks.
    fn chunk_text(&self, text: &str, target_len: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();

        for line in text.lines() {
            if current_chunk.len() + line.len() > target_len && !current_chunk.is_empty() {
                chunks.push(current_chunk.clone());
                current_chunk.clear();
            }
            current_chunk.push_str(line);
            current_chunk.push('\n');
        }
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }
        chunks
    }
}

/// Computes the cosine similarity between two f32 vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot_product = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        dot_product += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot_product / (norm_a.sqrt() * norm_b.sqrt())
}
