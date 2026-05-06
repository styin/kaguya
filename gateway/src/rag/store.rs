//! SQLite + FTS5 storage layer for RAG memories.

use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

/// Tokenize a raw user query into an FTS5-safe MATCH expression.
///
/// FTS5's MATCH language treats `:`, `"`, `+`, `-`, `(`, `)`, `*`, `^` and
/// the keywords `AND` / `OR` / `NOT` as syntax. Real user input contains
/// these all the time (`"C++"`, `"example.com:8080"`, `"def foo()"`) and
/// would otherwise raise parse errors that we silently convert to empty
/// retrieval results.
///
/// Strategy: split on **any** non-alphanumeric character (so `"example.com"`
/// becomes `["example", "com"]`, matching how FTS5's `unicode61` tokenizer
/// indexed the stored content). Wrap each token in double quotes so FTS5
/// treats it as a phrase literal — that keeps the token verbatim and
/// avoids the chance of one of them being interpreted as `AND`/`OR`/`NOT`.
/// Join with `OR` so partial matches still surface candidates; BM25
/// ranking promotes docs that match many query terms to the top.
fn sanitize_fts5_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    tokens.join(" OR ")
}

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
        // FTS5 MATCH has its own query mini-language layered on top of the
        // bound parameter (column filters via `:`, phrase quoting via `"`,
        // boolean operators `+`/`-`/`AND`/`OR`/`NOT`, prefix `*`, group `()`).
        // Passing raw user utterances straight in raises FTS5 parse errors
        // for many ordinary inputs (e.g. "C++", "example.com:8080",
        // unbalanced quotes). Tokenize and quote each token so FTS5 treats
        // them as literal terms.
        let sanitized = sanitize_fts5_query(query);
        if sanitized.is_empty() {
            return vec![];
        }

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
        let rows = match stmt.query_map(params![sanitized, limit as i64], |row| {
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

    /// Render the long-term-persona memory_md document delivered via
    /// `UpdatePersona`. `max_chars_per_entry` caps the "Recent Context"
    /// rows at output time — the store itself always holds full content.
    pub async fn export_as_markdown(&self, max_chars_per_entry: Option<usize>) -> String {
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
                    let line = match max_chars_per_entry {
                        Some(cap) if c.chars().count() > cap => {
                            crate::rag::truncate_chars(&c, cap).to_string()
                        }
                        _ => c,
                    };
                    md.push_str(&format!("- {line}\n"));
                }
            }
        }
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Allocate a fresh on-disk SQLite path under the OS temp dir.
    /// Each test gets a unique file; deletion is best-effort on Drop via
    /// `_TempDb`. We don't use `:memory:` because `RagStore::open` calls
    /// `create_dir_all(parent)` which has surprising semantics on an empty
    /// parent path on some platforms.
    struct _TempDb(std::path::PathBuf);
    impl _TempDb {
        fn new() -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!("kaguya-test-{}.db", uuid::Uuid::new_v4()));
            Self(p)
        }
    }
    impl Drop for _TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
        }
    }

    fn entry(content: &str, mem_type: MemoryType) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            content: content.to_string(),
            memory_type: mem_type,
            source: "test".to_string(),
            created_at: chrono::Utc::now().timestamp_millis(),
        }
    }

    // ── P0-1: BM25 query sanitization ────────────────────────────────────

    #[tokio::test]
    async fn bm25_returns_results_for_punctuated_query() {
        // Real-world input: a user asking about C++ build times. The raw
        // string contains `+` chars that FTS5 MATCH parses as boolean
        // operators, raising a parse error. The current `search_bm25`
        // swallows the error and returns []. This test asserts retrieval
        // SUCCEEDS — i.e. the implementation must sanitize / tokenize the
        // query before binding it to MATCH.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "I prefer Rust over C++ for the trading platform; build times matter",
            MemoryType::Preference,
        ))
        .await
        .unwrap();

        let r = s.search_bm25("C++ build times", 10).await;
        assert!(
            !r.is_empty(),
            "BM25 must not silently fail on '+' chars; \
             expected at least one hit on 'build' / 'times'"
        );
    }

    #[tokio::test]
    async fn bm25_returns_results_for_colon_query() {
        // Colon is FTS5's column-filter syntax (`title:foo`). Natural
        // queries like "what's at example.com:8080" or "foo:bar" should
        // not blow up retrieval.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "User: example.com is the canonical demo domain",
            MemoryType::Fact,
        ))
        .await
        .unwrap();

        let r = s.search_bm25("example.com:8080 demo", 10).await;
        assert!(
            !r.is_empty(),
            "BM25 must handle ':' in user input; expected hit on 'example' / 'demo'"
        );
    }

    #[tokio::test]
    async fn bm25_returns_results_for_quoted_query() {
        // Unbalanced quotes from a transcript like 'he said "hello world'
        // currently break FTS5 parsing.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "The greeting was hello world spoken in jest",
            MemoryType::Conversation,
        ))
        .await
        .unwrap();

        let r = s.search_bm25(r#"he said "hello world"#, 10).await;
        assert!(
            !r.is_empty(),
            "BM25 must tolerate unbalanced quotes in user input"
        );
    }

    #[tokio::test]
    async fn bm25_returns_results_for_paren_query() {
        // Code-shaped query: parentheses, colons. Common in technical chat.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "Function foo returns an integer in the project",
            MemoryType::Fact,
        ))
        .await
        .unwrap();

        let r = s.search_bm25("def foo(): return 42", 10).await;
        assert!(
            !r.is_empty(),
            "BM25 must tolerate parens and colons in user input"
        );
    }

    #[tokio::test]
    async fn bm25_clean_query_still_works() {
        // Sanity: a perfectly clean ASCII query should still match. Guards
        // against the sanitizer being so aggressive it strips everything.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "User prefers Rust for systems work",
            MemoryType::Preference,
        ))
        .await
        .unwrap();

        let r = s.search_bm25("rust systems", 10).await;
        assert!(!r.is_empty(), "clean queries must still match");
    }

    // ── P0-2: export_as_markdown includes Preference and Project ─────────

    #[tokio::test]
    async fn export_includes_preference_rows() {
        // Strategy B contract: the `## User Preferences` section surfaces
        // every Preference memory. Currently `export_as_markdown` only
        // includes ('conversation','fact'); preferences are written but
        // dead-stored.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "User preference: I like Rust over Python",
            MemoryType::Preference,
        ))
        .await
        .unwrap();

        let md = s.export_as_markdown(None).await;
        assert!(
            md.contains("I like Rust"),
            "preference row must surface in memory_md export; got:\n{md}"
        );
        assert!(
            md.contains("## User Preferences"),
            "expected '## User Preferences' section header; got:\n{md}"
        );
    }

    #[tokio::test]
    async fn export_includes_project_rows() {
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "Project: trading-platform v2 migration",
            MemoryType::Project,
        ))
        .await
        .unwrap();

        let md = s.export_as_markdown(None).await;
        assert!(
            md.contains("trading-platform"),
            "project row must surface in memory_md export; got:\n{md}"
        );
        assert!(
            md.contains("## Active Projects"),
            "expected '## Active Projects' section header; got:\n{md}"
        );
    }

    #[tokio::test]
    async fn export_keeps_recent_context_for_conversation_and_fact() {
        // Don't regress the existing behavior: the ## Recent Context
        // section should still pull conversation / fact rows.
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry(
            "Q: what's the time → A: it's 3pm",
            MemoryType::Conversation,
        ))
        .await
        .unwrap();
        s.insert_memory(&entry(
            "User: my name is Sebastian → Noted",
            MemoryType::Fact,
        ))
        .await
        .unwrap();

        let md = s.export_as_markdown(None).await;
        assert!(md.contains("3pm"));
        assert!(md.contains("Sebastian"));
        assert!(md.contains("## Recent Context"));
    }

    #[tokio::test]
    async fn export_does_not_mix_preferences_into_recent_context() {
        // Section discipline: Preferences belong in their own section, not
        // jumbled into Recent Context. (This guards against a regression
        // where someone "fixes" P0-2 by just relaxing the WHERE clause
        // on Recent Context — Strategy A — instead of separating sections.)
        let db = _TempDb::new();
        let s = RagStore::open(&db.0).unwrap();
        s.insert_memory(&entry("User preference: dark mode", MemoryType::Preference))
            .await
            .unwrap();

        let md = s.export_as_markdown(None).await;
        // The preference must be UNDER the User Preferences header, not
        // under Recent Context. Find the section boundaries and the
        // preference row.
        let pref_idx = md.find("dark mode").expect("pref present");
        let pref_section_idx = md.find("## User Preferences").expect("pref header");
        let recent_section_idx = md.find("## Recent Context").expect("recent header");

        assert!(
            pref_idx > pref_section_idx,
            "preference must appear AFTER its own section header"
        );
        if pref_section_idx < recent_section_idx {
            assert!(
                pref_idx < recent_section_idx,
                "preference must NOT spill into Recent Context section"
            );
        }
    }
}
