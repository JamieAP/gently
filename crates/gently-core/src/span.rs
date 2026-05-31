//! Internal span representation. The OTLP/JSON wire form lives in [`crate::otlp`].

use crate::ids::{SpanId, TraceId};

/// OpenTelemetry span kind. Tool calls are client spans (the agent calls out);
/// turns and the session root are internal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    /// OTLP numeric encoding (0 unspecified, 1 internal, 2 server, 3 client, ...).
    pub fn as_otlp(self) -> u8 {
        match self {
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
            SpanKind::Producer => 4,
            SpanKind::Consumer => 5,
        }
    }

    /// Inverse of [`SpanKind::as_otlp`]; unknown codes map to `Internal`.
    pub fn from_otlp(code: u8) -> Self {
        match code {
            2 => SpanKind::Server,
            3 => SpanKind::Client,
            4 => SpanKind::Producer,
            5 => SpanKind::Consumer,
            _ => SpanKind::Internal,
        }
    }
}

/// Span status. `Error` carries an optional human message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Unset,
    Ok,
    Error(Option<String>),
}

impl Status {
    /// OTLP status code (0 unset, 1 ok, 2 error).
    pub fn code(&self) -> u8 {
        match self {
            Status::Unset => 0,
            Status::Ok => 1,
            Status::Error(_) => 2,
        }
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Status::Error(Some(m)) => Some(m),
            _ => None,
        }
    }
}

/// Per-session resource attributes attached to every span batch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resource {
    pub session_id: String,
    pub harness: String,
    pub cwd: String,
    pub host: String,
    pub os: String,
    pub version: String,
}

impl Resource {
    /// Build a resource, filling host/os/version from the environment.
    pub fn new(
        session_id: impl Into<String>,
        harness: impl Into<String>,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            harness: harness.into(),
            cwd: cwd.into(),
            host: hostname(),
            os: std::env::consts::OS.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A fully-formed span ready to be encoded as OTLP and shipped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Span {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub name: String,
    pub kind: SpanKind,
    pub start_unix_nano: u64,
    pub end_unix_nano: u64,
    pub status: Status,
    /// String-valued span attributes (keys are namespaced `gently.*`).
    pub attributes: Vec<(String, String)>,
}
