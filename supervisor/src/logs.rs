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
const ESC: char = '\u{1b}';

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
        let line = strip_ansi(&line.into());
        if line.trim().is_empty() || is_transient_status_line(&line) {
            return;
        }
        let entry = LogEntry {
            id: self.next_id.fetch_add(1, Ordering::SeqCst),
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            source: normalize_source(&source.into()),
            stream: stream.into(),
            line,
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

fn is_transient_status_line(line: &str) -> bool {
    let trimmed = line.trim();
    if let Some(first) = trimmed.chars().next() {
        if ('\u{2800}'..='\u{28ff}').contains(&first) {
            return true;
        }
    }
    let trimmed = trimmed
        .trim_start_matches(['\\', '|', '/', '-'])
        .trim()
        .to_ascii_lowercase();
    matches!(trimmed.as_str(), "recording" | "speak now")
}

fn strip_ansi(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != ESC {
            output.push(ch);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_strips_ansi_sequences() {
        let logs = LogStore::new();
        logs.push(
            "gateway",
            "stdout",
            "\u{1b}[2m2026-05-29\u{1b}[0m \u{1b}[32mINFO\u{1b}[0m ready",
        );

        let entries = logs.since(0);
        assert_eq!(entries[0].line, "2026-05-29 INFO ready");
    }

    #[test]
    fn push_drops_voice_spinner_lines() {
        let logs = LogStore::new();
        logs.push("talker", "stdout", "\\ speak now");
        logs.push("talker", "stdout", "real log");

        let entries = logs.since(0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].line, "real log");
    }
}
