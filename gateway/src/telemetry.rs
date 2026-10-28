#[derive(Clone)]
pub struct TelemetryClient {
    base_url: String,
    source: String,
    http: reqwest::Client,
    session: crate::proto::SessionRef,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TelemetryPayload {
    source: String,
    kind: String,
    instance_id: String,
    session_id: String,
    allocation_id: Option<String>,
    generation_id: Option<String>,
    request_id: Option<String>,
    fields: serde_json::Value,
}

impl TelemetryClient {
    pub fn new(
        base_url: impl Into<String>,
        source: impl Into<String>,
        session: crate::proto::SessionRef,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            source: source.into(),
            http: reqwest::Client::new(),
            session,
        }
    }

    pub fn emit(
        &self,
        kind: impl Into<String>,
        originating_session_id: Option<String>,
        generation_id: Option<String>,
        request_id: Option<String>,
        fields: serde_json::Value,
    ) {
        let (instance_id, session_id) =
            match crate::instance::validate_session_ref(Some(&self.session)) {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(%error, "dropping telemetry with invalid configured scope");
                    return;
                }
            };
        if originating_session_id
            .as_deref()
            .is_some_and(|id| id != session_id)
        {
            tracing::warn!("dropping telemetry with mismatched originating session correlation");
            return;
        }
        let payload = TelemetryPayload {
            source: self.source.clone(),
            kind: kind.into(),
            instance_id: instance_id.to_owned(),
            session_id: session_id.to_owned(),
            allocation_id: None,
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
