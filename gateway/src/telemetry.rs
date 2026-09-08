#[derive(Clone)]
pub struct TelemetryClient {
    base_url: String,
    source: String,
    http: reqwest::Client,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TelemetryPayload {
    source: String,
    kind: String,
    conversation_id: Option<String>,
    turn_id: Option<String>,
    request_id: Option<String>,
    fields: serde_json::Value,
}

impl TelemetryClient {
    pub fn new(base_url: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            source: source.into(),
            http: reqwest::Client::new(),
        }
    }

    pub fn emit(
        &self,
        kind: impl Into<String>,
        conversation_id: Option<String>,
        turn_id: Option<String>,
        request_id: Option<String>,
        fields: serde_json::Value,
    ) {
        let payload = TelemetryPayload {
            source: self.source.clone(),
            kind: kind.into(),
            conversation_id,
            turn_id,
            request_id,
            fields,
        };
        let http = self.http.clone();
        let url = format!("{}/api/telemetry/events", self.base_url);
        tokio::spawn(async move {
            if let Err(error) = http.post(url).json(&payload).send().await {
                tracing::debug!(%error, "telemetry event upload failed");
            }
        });
    }
}
