use std::fmt;

#[derive(Clone)]
pub struct ResolvedStream {
    pub local_input: bool,
    transcoded: bool,
    pub content_type: Option<String>,
    uri: String,
    redacted_uri: String,
    trust_invalid_certificate: bool,
    time_seek: bool,
    time_offset_millis: Option<u64>,
    resource: Option<std::sync::Arc<dyn Send + Sync>>,
}

impl PartialEq for ResolvedStream {
    fn eq(&self, other: &Self) -> bool {
        self.uri == other.uri
            && self.local_input == other.local_input
            && self.transcoded == other.transcoded
            && self.redacted_uri == other.redacted_uri
            && self.trust_invalid_certificate == other.trust_invalid_certificate
            && self.time_seek == other.time_seek
            && self.time_offset_millis == other.time_offset_millis
            && self.content_type == other.content_type
    }
}

impl Eq for ResolvedStream {}

impl ResolvedStream {
    pub fn new(uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            local_input: uri.starts_with("file:") || uri.starts_with("content:"),
            redacted_uri: redact_sensitive_uri(&uri),
            transcoded: false,
            uri,
            trust_invalid_certificate: false,
            time_seek: false,
            time_offset_millis: None,
            resource: None,
            content_type: None,
        }
    }

    pub fn with_redacted(uri: impl Into<String>, redacted_uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            local_input: uri.starts_with("file:") || uri.starts_with("content:"),
            transcoded: false,
            uri,
            redacted_uri: redacted_uri.into(),
            trust_invalid_certificate: false,
            time_seek: false,
            time_offset_millis: None,
            resource: None,
            content_type: None,
        }
    }

    pub fn with_trust_invalid_certificate(mut self, trust: bool) -> Self {
        self.trust_invalid_certificate = trust;
        self
    }

    pub fn with_transcoding(mut self, transcoded: bool) -> Self {
        self.transcoded = transcoded;
        self
    }

    pub fn transcoded(&self) -> bool {
        self.transcoded
    }

    pub fn with_content_type(mut self, content_type: Option<String>) -> Self {
        self.content_type = content_type;
        self
    }

    /// Retain temporary input or its serving endpoint while this stream is in use.
    /// Resource ownership does not participate in stream identity or equality.
    pub fn with_resource(mut self, resource: std::sync::Arc<dyn Send + Sync>) -> Self {
        self.resource = Some(resource);
        self
    }

    /// The HTTP input accepts source-time start_ms and optional end_ms bounds.
    pub fn with_time_seek(mut self) -> Self {
        self.time_seek = true;
        self
    }

    pub fn time_seekable(&self) -> bool {
        self.time_seek
    }
    pub fn time_offset_millis(&self) -> Option<u64> {
        self.time_offset_millis
    }

    pub fn reopened_at(&self, logical_millis: u64) -> Result<Self, url::ParseError> {
        let mut stream = self.clone();
        if !self.time_seek {
            return Ok(stream);
        }
        let start = logical_millis;
        let mut uri = url::Url::parse(self.uri())?;
        let pairs = uri
            .query_pairs()
            .filter(|(key, _)| key != "start_ms" && key != "end_ms")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        uri.set_query(None);
        uri.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("start_ms", &start.to_string());
        stream.uri = uri.into();
        stream.redacted_uri = redact_sensitive_uri(&stream.uri);
        stream.time_offset_millis = Some(start);
        Ok(stream)
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn redacted_uri(&self) -> &str {
        &self.redacted_uri
    }
    pub fn trust_invalid_certificate(&self) -> bool {
        self.trust_invalid_certificate
    }
}

impl fmt::Debug for ResolvedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedStream")
            .field("uri", &self.redacted_uri)
            .finish()
    }
}

fn redact_sensitive_uri(uri: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(uri) else {
        return "<redacted>".into();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_fragment(None);
    let query = parsed.query().map(|query| {
        query
            .split('&')
            .map(|pair| format!("{}=<redacted>", pair.split('=').next().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("&")
    });
    parsed.set_query(None);
    let mut redacted = parsed.to_string();
    if let Some(query) = query {
        redacted.push('?');
        redacted.push_str(&query);
    }
    redacted
}
