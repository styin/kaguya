//! SQLite + FTS5 storage layer for RAG memories.

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
    pub source: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryType {
    Conversation,
    Fact,
    Preference,
    Project,
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

pub struct BM25Result {
    pub id: String,
    pub content: String,
    pub rank: f64,
}

pub struct RagStore {
    conn: Arc<Mutex<Connection>>,
}

impl RagStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(p)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id          TEXT PRIMARY KEY,
                content     TEXT NOT NULL,
                memory_type TEXT NOT NULL,
                source      TEXT DEFAULT '',
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content, memory_type,
                content='memories', content_rowid='rowid',
                tokenize='porter unicode61'
            );
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
            CREATE TABLE IF NOT EXISTS embeddings (
                memory_id TEXT PRIMARY KEY REFERENCES memories(id),
                vector    BLOB NOT NULL,
                model     TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS user_profile (
                key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS projects (
                name TEXT PRIMARY KEY, description TEXT DEFAULT '',
                metadata TEXT DEFAULT '{}', updated_at INTEGER NOT NULL
            );"
        )?;
        info!("RAG store opened: {}", p.display());
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

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

    pub async fn search_bm25(&self, query: &str, limit: usize) -> Vec<BM25Result> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT m.id, m.content, rank
             FROM memories_fts fts
             JOIN memories m ON m.rowid = fts.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map(params![query, limit as i64], |row| {
            Ok(BM25Result {
                id: row.get(0)?,
                content: row.get(1)?,
                rank: row.get(2)?,
            })
        }) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    pub async fn store_embedding(
        &self,
        memory_id: &str,
        vector: &[f32],
        model: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        let blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT OR REPLACE INTO embeddings (memory_id, vector, model, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![memory_id, blob, model, chrono::Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    pub async fn all_embeddings(&self) -> Vec<(String, Vec<f32>)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare("SELECT memory_id, vector FROM embeddings") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            let vec: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok((id, vec))
        }) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    pub async fn get_memory(&self, id: &str) -> Option<MemoryEntry> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, content, memory_type, source, created_at FROM memories WHERE id = ?1",
            params![id],
            |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    memory_type: MemoryType::from_str(&row.get::<_, String>(2)?),
                    source: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .ok()
    }

    pub async fn unembedded_ids(&self) -> Vec<(String, String)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT m.id, m.content FROM memories m
             LEFT JOIN embeddings e ON m.id = e.memory_id
             WHERE e.memory_id IS NULL
             ORDER BY m.created_at DESC LIMIT 100"
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    pub async fn export_as_markdown(&self) -> String {
        let conn = self.conn.lock().await;
        let mut md = String::from("## User Profile\n\n");

        if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM user_profile") {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    md.push_str(&format!("- {}: {}\n", row.0, row.1));
                }
            }
        }

        md.push_str("\n## Project Context\n\n");
        if let Ok(mut stmt) = conn.prepare(
            "SELECT name, description FROM projects ORDER BY updated_at DESC LIMIT 20",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    md.push_str(&format!("- {}: {}\n", row.0, row.1));
                }
            }
        }

        md.push_str("\n## Recent Context\n\n");
        if let Ok(mut stmt) = conn.prepare(
            "SELECT content FROM memories WHERE memory_type IN ('conversation','fact')
             ORDER BY created_at DESC LIMIT 30",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for c in rows.flatten() {
                    md.push_str(&format!("- {c}\n"));
                }
            }
        }
        md
    }
}
