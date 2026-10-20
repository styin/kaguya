#[derive(Clone)]
pub struct TelemetryClient {
    base_url: String,
    source: String,
    http: reqwest::Client,
    instance_context: Option<crate::instance::InstanceIdentity>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TelemetryPayload {
    source: String,
    kind: String,
    instance_id: Option<String>,
    session_id: Option<String>,
    allocation_id: Option<String>,
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
            instance_context: None,
        }
    }

    pub fn with_instance_context(
        mut self,
        instance_context: crate::instance::InstanceIdentity,
    ) -> Self {
        self.instance_context = Some(instance_context);
        self
    }

    pub fn emit(
        &self,
        kind: impl Into<String>,
        conversation_id: Option<String>,
        turn_id: Option<String>,
        request_id: Option<String>,
        fields: serde_json::Value,
    ) {
        if let (Some(instance_context), Some(conversation_id)) =
            (&self.instance_context, &conversation_id)
        {
            if conversation_id != &instance_context.default_session_id {
                tracing::warn!(%conversation_id, "dropping telemetry with mismatched originating session correlation");
                return;
            }
        }
        let payload = TelemetryPayload {
            source: self.source.clone(),
            kind: kind.into(),
            instance_id: self
                .instance_context
                .as_ref()
                .map(|instance_context| instance_context.instance_id.clone()),
            session_id: self
                .instance_context
                .as_ref()
                .map(|instance_context| instance_context.default_session_id.clone()),
            allocation_id: None,
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
