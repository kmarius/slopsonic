use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedStream {
    pub local_input: bool,
    pub content_type: Option<String>,
    uri: String,
    redacted_uri: String,
    trust_invalid_certificate: bool,
}

impl ResolvedStream {
    pub fn new(uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            local_input: uri.starts_with("file:"),
            redacted_uri: redact_sensitive_uri(&uri),
            uri,
            trust_invalid_certificate: false,
            content_type: None,
        }
    }

    pub fn with_redacted(uri: impl Into<String>, redacted_uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            local_input: uri.starts_with("file:"),
            uri,
            redacted_uri: redacted_uri.into(),
            trust_invalid_certificate: false,
            content_type: None,
        }
    }

    pub fn with_trust_invalid_certificate(mut self, trust: bool) -> Self {
        self.trust_invalid_certificate = trust;
        self
    }

    pub fn with_content_type(mut self, content_type: Option<String>) -> Self {
        self.content_type = content_type;
        self
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
