use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

const MAX_TELEMETRY_EVENTS: usize = 10_000;
const AGGREGATOR_QUEUE_CAP: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEvent {
    pub id: u64,
    pub timestamp: String,
    pub source: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingTelemetryEvent {
    pub source: String,
    pub kind: String,
    pub conversation_id: Option<String>,
    pub turn_id: Option<String>,
    pub request_id: Option<String>,
    #[serde(default)]
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryMetrics {
    pub total_events: u64,
    pub events_by_kind: BTreeMap<String, u64>,
    pub rag: RagMetrics,
    pub talker: TalkerMetrics,
    pub sandbox: SandboxMetrics,
    pub process: ProcessMetrics,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RagMetrics {
    pub retrievals: u64,
    pub hits: u64,
    pub total_duration_ms: u64,
    pub last_duration_ms: Option<u64>,
    pub last_fused_hits: Option<u64>,
    pub last_top_score: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkerMetrics {
    pub dispatches: u64,
    pub first_outputs: u64,
    pub first_sentences: u64,
    pub total_first_output_ms: u64,
    pub total_first_sentence_ms: u64,
    pub last_first_output_ms: Option<u64>,
    pub last_first_sentence_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxMetrics {
    pub executions: u64,
    pub timeouts: u64,
    pub failures: u64,
    pub total_duration_ms: u64,
    pub last_duration_ms: Option<u64>,
    pub last_backend: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessMetrics {
    pub samples: u64,
    pub last_sample_id: Option<u64>,
    pub last_sampled_processes: u64,
    pub last_total_rss_bytes: u64,
    pub last_total_virtual_memory_bytes: u64,
    pub last_total_cpu_percent: f64,
    pub peak_total_rss_bytes: u64,
    pub peak_process_rss_bytes: u64,
}

#[derive(Clone)]
pub struct TelemetryHub {
    entries: Arc<Mutex<VecDeque<TelemetryEvent>>>,
    metrics: Arc<Mutex<TelemetryMetrics>>,
    next_id: Arc<AtomicU64>,
    tx: broadcast::Sender<TelemetryEvent>,
    agg_tx: mpsc::Sender<TelemetryEvent>,
}

impl TelemetryHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        let (agg_tx, mut agg_rx) = mpsc::channel::<TelemetryEvent>(AGGREGATOR_QUEUE_CAP);
        let metrics = Arc::new(Mutex::new(TelemetryMetrics::default()));
        let metrics_worker = Arc::clone(&metrics);
        tokio::spawn(async move {
            while let Some(event) = agg_rx.recv().await {
                let mut metrics = metrics_worker
                    .lock()
                    .expect("telemetry metrics lock poisoned");
                update_metrics(&mut metrics, &event);
            }
        });

        Self {
            entries: Arc::new(Mutex::new(VecDeque::new())),
            metrics,
            next_id: Arc::new(AtomicU64::new(1)),
            tx,
            agg_tx,
        }
    }

    pub async fn emit_incoming(&self, incoming: IncomingTelemetryEvent) -> TelemetryEvent {
        self.emit(
            incoming.source,
            incoming.kind,
            incoming.conversation_id,
            incoming.turn_id,
            incoming.request_id,
            incoming.fields,
        )
        .await
    }

    pub async fn emit(
        &self,
        source: impl Into<String>,
        kind: impl Into<String>,
        conversation_id: Option<String>,
        turn_id: Option<String>,
        request_id: Option<String>,
        fields: serde_json::Value,
    ) -> TelemetryEvent {
        let event = TelemetryEvent {
            id: self.next_id.fetch_add(1, Ordering::SeqCst),
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            source: source.into(),
            kind: kind.into(),
            conversation_id,
            turn_id,
            request_id,
            fields,
        };

        {
            let mut entries = self.entries.lock().expect("telemetry lock poisoned");
            entries.push_back(event.clone());
            while entries.len() > MAX_TELEMETRY_EVENTS {
                entries.pop_front();
            }
        }

        let _ = self.tx.send(event.clone());
        let _ = self.agg_tx.try_send(event.clone());
        event
    }

    pub fn since(&self, since_id: u64) -> Vec<TelemetryEvent> {
        let entries = self.entries.lock().expect("telemetry lock poisoned");
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

    pub fn subscribe(&self) -> broadcast::Receiver<TelemetryEvent> {
        self.tx.subscribe()
    }

    pub fn metrics(&self) -> TelemetryMetrics {
        self.metrics
            .lock()
            .expect("telemetry metrics lock poisoned")
            .clone()
    }
}

impl Default for TelemetryHub {
    fn default() -> Self {
        Self::new()
    }
}

fn update_metrics(metrics: &mut TelemetryMetrics, event: &TelemetryEvent) {
    metrics.total_events += 1;
    *metrics
        .events_by_kind
        .entry(event.kind.clone())
        .or_insert(0) += 1;

    match event.kind.as_str() {
        "rag.retrieve.completed" => update_rag_metrics(&mut metrics.rag, &event.fields),
        "talker.dispatch.started" => metrics.talker.dispatches += 1,
        "talker.first_output" => {
            metrics.talker.first_outputs += 1;
            if let Some(ms) = field_u64(&event.fields, "duration_ms") {
                metrics.talker.total_first_output_ms += ms;
                metrics.talker.last_first_output_ms = Some(ms);
            }
        }
        "talker.first_sentence" => {
            metrics.talker.first_sentences += 1;
            if let Some(ms) = field_u64(&event.fields, "duration_ms") {
                metrics.talker.total_first_sentence_ms += ms;
                metrics.talker.last_first_sentence_ms = Some(ms);
            }
        }
        "sandbox.exec.completed" => update_sandbox_metrics(&mut metrics.sandbox, &event.fields),
        "process.resource.sample" => update_process_metrics(&mut metrics.process, &event.fields),
        _ => {}
    }
}

fn update_rag_metrics(metrics: &mut RagMetrics, fields: &serde_json::Value) {
    metrics.retrievals += 1;
    let fused_hits = field_u64(fields, "fused_hits").unwrap_or(0);
    if fused_hits > 0 {
        metrics.hits += 1;
    }
    if let Some(ms) = field_u64(fields, "duration_ms") {
        metrics.total_duration_ms += ms;
        metrics.last_duration_ms = Some(ms);
    }
    metrics.last_fused_hits = Some(fused_hits);
    metrics.last_top_score = fields.get("top_score").and_then(|value| value.as_f64());
}

fn update_sandbox_metrics(metrics: &mut SandboxMetrics, fields: &serde_json::Value) {
    metrics.executions += 1;
    if fields
        .get("timed_out")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        metrics.timeouts += 1;
    }
    let exit_code = fields
        .get("exit_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if exit_code != 0 || fields.get("error").is_some_and(|value| !value.is_null()) {
        metrics.failures += 1;
    }
    if let Some(ms) = field_u64(fields, "duration_ms") {
        metrics.total_duration_ms += ms;
        metrics.last_duration_ms = Some(ms);
    }
    metrics.last_backend = fields
        .get("backend")
        .and_then(|value| value.as_str())
        .map(str::to_string);
}

fn update_process_metrics(metrics: &mut ProcessMetrics, fields: &serde_json::Value) {
    metrics.samples += 1;
    let sample_id = field_u64(fields, "sample_id");
    if sample_id != metrics.last_sample_id {
        metrics.last_sample_id = sample_id;
        metrics.last_sampled_processes = 0;
        metrics.last_total_rss_bytes = 0;
        metrics.last_total_virtual_memory_bytes = 0;
        metrics.last_total_cpu_percent = 0.0;
    }
    let rss = field_u64(fields, "rss_bytes").unwrap_or(0);
    let virtual_memory = field_u64(fields, "virtual_memory_bytes").unwrap_or(0);
    let cpu = fields
        .get("cpu_percent")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);

    metrics.last_sampled_processes += 1;
    metrics.last_total_rss_bytes += rss;
    metrics.last_total_virtual_memory_bytes += virtual_memory;
    metrics.last_total_cpu_percent += cpu;
    metrics.peak_process_rss_bytes = metrics.peak_process_rss_bytes.max(rss);
    metrics.peak_total_rss_bytes = metrics
        .peak_total_rss_bytes
        .max(metrics.last_total_rss_bytes);
}

fn field_u64(fields: &serde_json::Value, key: &str) -> Option<u64> {
    fields.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|v| v.try_into().ok()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hub_stores_streams_and_aggregates_events() {
        let hub = TelemetryHub::new();
        let mut rx = hub.subscribe();

        let event = hub
            .emit(
                "gateway",
                "rag.retrieve.completed",
                Some("conversation".into()),
                Some("turn".into()),
                None,
                serde_json::json!({
                    "duration_ms": 12,
                    "fused_hits": 3,
                    "top_score": 0.42,
                }),
            )
            .await;

        assert_eq!(event.id, 1);
        assert_eq!(hub.since(0).len(), 1);
        assert_eq!(rx.recv().await.unwrap().kind, "rag.retrieve.completed");

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let metrics = hub.metrics();
        assert_eq!(metrics.total_events, 1);
        assert_eq!(metrics.rag.retrievals, 1);
        assert_eq!(metrics.rag.hits, 1);
        assert_eq!(metrics.rag.last_duration_ms, Some(12));
    }

    #[tokio::test]
    async fn process_resource_samples_reset_totals_per_sample_id() {
        let hub = TelemetryHub::new();
        hub.emit(
            "supervisor",
            "process.resource.sample",
            None,
            None,
            None,
            serde_json::json!({
                "sample_id": 1,
                "process": "gateway",
                "cpu_percent": 10.0,
                "rss_bytes": 100,
                "virtual_memory_bytes": 1000,
            }),
        )
        .await;
        hub.emit(
            "supervisor",
            "process.resource.sample",
            None,
            None,
            None,
            serde_json::json!({
                "sample_id": 1,
                "process": "talker",
                "cpu_percent": 20.0,
                "rss_bytes": 200,
                "virtual_memory_bytes": 2000,
            }),
        )
        .await;
        hub.emit(
            "supervisor",
            "process.resource.sample",
            None,
            None,
            None,
            serde_json::json!({
                "sample_id": 2,
                "process": "gateway",
                "cpu_percent": 7.0,
                "rss_bytes": 70,
                "virtual_memory_bytes": 700,
            }),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let metrics = hub.metrics();
        assert_eq!(metrics.process.samples, 3);
        assert_eq!(metrics.process.last_sample_id, Some(2));
        assert_eq!(metrics.process.last_sampled_processes, 1);
        assert_eq!(metrics.process.last_total_rss_bytes, 70);
        assert_eq!(metrics.process.peak_total_rss_bytes, 300);
        assert_eq!(metrics.process.peak_process_rss_bytes, 200);
    }
}
