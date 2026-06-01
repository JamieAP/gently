//! Harness adapters: translate a coding harness's raw hook payloads into
//! harness-agnostic [`SpanOp`]s, then [`apply`](apply::apply) those ops against
//! the local [`Store`](gently_store::Store) to produce completed spans.
//!
//! The split keeps parsing pure and trivially testable (raw JSON in, ops out)
//! while all stateful concerns - turn counters, parent resolution, deterministic
//! id derivation, timestamps - live in one applier. Adding Codex or Cursor means
//! a new [`Harness`] impl and nothing else.

mod apply;
mod claude;
mod codex;
mod hooks;

pub use apply::apply;
pub use claude::ClaudeCode;
pub use codex::Codex;

use gently_core::Status;

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid payload: {0}")]
    Invalid(String),
}

/// Maps a harness's raw hook payload into span operations.
///
/// Implementations are pure: given one stdin event they return the session
/// context plus zero or more [`SpanOp`]s, without touching any store or clock.
pub trait Harness {
    /// Stable harness identifier, recorded as a resource attribute.
    fn name(&self) -> &'static str;
    /// Parse one raw hook event into its session context and span operations.
    fn parse(&self, raw: &serde_json::Value) -> Result<Parsed, HarnessError>;
}

/// The session context plus operations extracted from one hook event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub session_id: String,
    pub cwd: String,
    /// The harness-reported transcript path, when the payload carries one
    /// (Claude's `transcript_path`). `None` for harnesses/events that omit it.
    pub transcript_path: Option<String>,
    pub ops: Vec<SpanOp>,
}

/// One harness-agnostic span operation. `Open*` variants record a start;
/// `Close*` variants pair with a prior open (by tool_use_id / agent_id / the
/// current turn) and emit a completed span; `Mark` emits an instant span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpanOp {
    OpenSession {
        attrs: Attrs,
    },
    CloseSession {
        status: Status,
        attrs: Attrs,
    },
    OpenTurn {
        attrs: Attrs,
    },
    CloseTurn {
        status: Status,
        attrs: Attrs,
    },
    OpenTool {
        tool_use_id: Option<String>,
        tool_name: String,
        attrs: Attrs,
    },
    CloseTool {
        tool_use_id: Option<String>,
        tool_name: String,
        status: Status,
        duration_ms: Option<u64>,
        attrs: Attrs,
    },
    OpenAgent {
        agent_id: String,
        parent_tool_use_id: Option<String>,
        attrs: Attrs,
    },
    CloseAgent {
        agent_id: String,
        status: Status,
        attrs: Attrs,
    },
    /// An instant (zero-duration) span for an unmodeled or point-in-time event.
    Mark {
        name: String,
        attrs: Attrs,
    },
}

/// String-valued span attributes, kept as ordered pairs for stable encoding.
pub type Attrs = Vec<(String, String)>;
