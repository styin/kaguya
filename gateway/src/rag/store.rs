//! RAG Storage Layer — SQLite + FTS5
//!
//! Three tables:
//! - memories: conversation memories, facts, preferences (with FTS5 index)
//! - user_profile: structured key-value pairs
//! - embeddings: incremental vector storage (BLOB)

use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub source: String,         // which turn/tool produced this
    pub created_at: i64,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryType {
    Conversation,   // 对话记忆 ("user asked about X, we discussed Y")
    Fact,           // 事实 ("Goedel pipeline runs at 2am")
    Preference,     // 偏好 ("user prefers concise responses")
    Project,        // 项目上下文 ("Kaguya is a voice-first AI")
}

impl MemoryType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Project => "project",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "fact" => Self::Fact,
            "preference" => Self::Preference,
            "project" => Self::Project,
            _ => Self::Conversation,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BM25Result {
    pub id: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub rank: f64,  // BM25 score from FTS5
}

pub struct RagStore {
    conn: Arc<Mutex<Connection>>,
}

impl RagStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        // Core memory table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id          TEXT PRIMARY KEY,
                content     TEXT NOT NULL,
                memory_type TEXT NOT NULL,
                source      TEXT DEFAULT '',
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            -- FTS5 full-text index for BM25 search
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                memory_type,
                content='memories',
                content_rowid='rowid',
                tokenize='porter unicode61'
            );

            -- Triggers to keep FTS in sync
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, content, memory_type)
                VALUES (new.rowid, new.content, new.memory_type);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, memory_type)
                VALUES ('delete', old.rowid, old.content, old.memory_type);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, memory_type)
                VALUES ('delete', old.rowid, old.content, old.memory_type);
                INSERT INTO memories_fts(rowid, content, memory_type)
                VALUES (new.rowid, new.content, new.memory_type);
            END;

            -- Embedding vectors (incremental, only for semantic search)
            CREATE TABLE IF NOT EXISTS embeddings (
                memory_id TEXT PRIMARY KEY REFERENCES memories(id),
                vector    BLOB NOT NULL,
                model     TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            -- Structured user profile
            CREATE TABLE IF NOT EXISTS user_profile (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );

            -- Project metadata
            CREATE TABLE IF NOT EXISTS projects (
                name        TEXT PRIMARY KEY,
                description TEXT DEFAULT '',
                metadata    TEXT DEFAULT '{}',
                updated_at  INTEGER NOT NULL
            );"
        )?;

        info!("RAG store initialized");
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Insert a new memory entry
    pub async fn insert_memory(&self, entry: &MemoryEntry) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT OR REPLACE INTO memories (id, content, memory_type, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entry.id, entry.content, entry.memory_type.as_str(),
                    entry.source, entry.created_at, now],
        )?;
        Ok(())
    }

    /// BM25 full-text search via FTS5
    pub async fn search_bm25(&self, query: &str, limit: usize) -> Vec<BM25Result> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.content, m.memory_type, rank
             FROM memories_fts fts
             JOIN memories m ON m.rowid = fts.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        ).unwrap();

        stmt.query_map(params![query, limit as i64], |row| {
            Ok(BM25Result {
                id: row.get(0)?,
                content: row.get(1)?,
                memory_type: MemoryType::from_str(&row.get::<_, String>(2)?),
                rank: row.get(3)?,
            })
        }).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Store embedding for a memory entry (incremental — only new entries)
    pub async fn store_embedding(&self, memory_id: &str, vector: &[f32], model: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        let blob = vector.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        conn.execute(
            "INSERT OR REPLACE INTO embeddings (memory_id, vector, model, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![memory_id, blob, model, chrono::Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    /// Get all embeddings for brute-force vector search
    /// Phase 2: Replace with HNSW index for >10K entries
    pub async fn all_embeddings(&self) -> Vec<(String, Vec<f32>)> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT e.memory_id, e.vector FROM embeddings e"
        ).unwrap();

        stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            let vec: Vec<f32> = blob.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok((id, vec))
        }).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Get memory content by ID (for vector search result hydration)
    pub async fn get_memory(&self, id: &str) -> Option<MemoryEntry> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, content, memory_type, source, created_at FROM memories WHERE id = ?1",
            params![id],
            |row| Ok(MemoryEntry {
                id: row.get(0)?,
                content: row.get(1)?,
                memory_type: MemoryType::from_str(&row.get::<_, String>(2)?),
                source: row.get(3)?,
                created_at: row.get(4)?,
                embedding: None,
            })
        ).ok()
    }

    /// Get IDs of memories that don't have embeddings yet (for incremental indexing)
    pub async fn unembedded_ids(&self) -> Vec<(String, String)> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.content FROM memories m
             LEFT JOIN embeddings e ON m.id = e.memory_id
             WHERE e.memory_id IS NULL
             ORDER BY m.created_at DESC
             LIMIT 100"
        ).unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Get all structured profile entries (injected directly into context)
    pub async fn get_profile(&self) -> Vec<(String, String)> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT key, value FROM user_profile").unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub async fn set_profile(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO user_profile (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![key, value, chrono::Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    /// 导出为 MEMORY.md 格式 (兼容旧的 PersonaConfig.memory_md)
    pub async fn export_as_markdown(&self) -> String {
        let profile = self.get_profile().await;
        let conn = self.conn.lock().await;

        let mut md = String::from("## User Profile\n\n");
        for (k, v) in &profile {
            md.push_str(&format!("- {k}: {v}\n"));
        }

        md.push_str("\n## Project Context\n\n");
        let mut stmt = conn.prepare(
            "SELECT name, description FROM projects ORDER BY updated_at DESC LIMIT 20"
        ).unwrap();
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                md.push_str(&format!("- {}: {}\n", row.0, row.1));
            }
        }

        md.push_str("\n## Recent Context\n\n");
        let mut stmt2 = conn.prepare(
            "SELECT content, created_at FROM memories
             WHERE memory_type IN ('conversation', 'fact')
             ORDER BY created_at DESC LIMIT 30"
        ).unwrap();
        if let Ok(rows) = stmt2.query_map([], |row| {
            Ok(row.get::<_, String>(0)?)
        }) {
            for content in rows.flatten() {
                md.push_str(&format!("- {content}\n"));
            }
        }

        md
    }
}