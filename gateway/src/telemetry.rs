use crate::session_scope::SessionScope;

#[derive(Clone)]
pub struct TelemetryClient {
    base_url: String,
    source: String,
    http: reqwest::Client,
    scope: SessionScope,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TelemetryPayload {
    source: String,
    kind: String,
    instance_id: String,
    session_id: String,
    generation_id: Option<String>,
    request_id: Option<String>,
    fields: serde_json::Value,
}

impl TelemetryClient {
    pub fn new(
        base_url: impl Into<String>,
        source: impl Into<String>,
        scope: SessionScope,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            source: source.into(),
            http: reqwest::Client::new(),
            scope,
        }
    }

    pub fn emit(
        &self,
        kind: impl Into<String>,
        generation_id: Option<String>,
        request_id: Option<String>,
        fields: serde_json::Value,
    ) {
        let payload = TelemetryPayload {
            source: self.source.clone(),
            kind: kind.into(),
            instance_id: self.scope.instance_id().to_string(),
            session_id: self.scope.session_id().to_string(),
            generation_id,
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
