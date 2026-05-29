use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use chrono::Utc;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::process::ManagedProcessLogLine;

const MAX_LOG_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub id: u64,
    pub timestamp: String,
    pub source: String,
    pub stream: String,
    pub line: String,
}

#[derive(Clone)]
pub struct LogStore {
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    next_id: Arc<AtomicU64>,
    tx: broadcast::Sender<LogEntry>,
}

impl LogStore {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            entries: Arc::new(Mutex::new(VecDeque::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            tx,
        }
    }

    pub fn push(
        &self,
        source: impl Into<String>,
        stream: impl Into<String>,
        line: impl Into<String>,
    ) {
        let entry = LogEntry {
            id: self.next_id.fetch_add(1, Ordering::SeqCst),
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            source: normalize_source(&source.into()),
            stream: stream.into(),
            line: line.into(),
        };

        {
            let mut entries = self.entries.lock().expect("log store lock poisoned");
            entries.push_back(entry.clone());
            while entries.len() > MAX_LOG_ENTRIES {
                entries.pop_front();
            }
        }

        let _ = self.tx.send(entry);
    }

    pub fn push_process_log(&self, line: ManagedProcessLogLine) {
        self.push(line.source, line.stream, line.line);
    }

    pub fn since(&self, since_id: u64) -> Vec<LogEntry> {
        let entries = self.entries.lock().expect("log store lock poisoned");
        if since_id == 0 {
            return entries
                .iter()
                .rev()
                .take(200)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
        }
        entries
            .iter()
            .filter(|entry| entry.id > since_id)
            .cloned()
            .collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_source(source: &str) -> String {
    match source {
        "gateway" | "gateway_standalone" | "kaguya_app" => "gateway".to_string(),
        "voice_stack" | "talker_standalone" => "talker".to_string(),
        other => other.to_string(),
    }
}
