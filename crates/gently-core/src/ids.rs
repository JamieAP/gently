//! Deterministic trace and span identifiers.
//!
//! Ids are pure functions of harness-provided identifiers (the session id plus a
//! logical key). This is what makes the local state reconstructible: a child
//! span can compute its `parent_span_id` without the parent span existing yet
//! and without any surviving local state, and the same logical span always maps
//! to the same id across separate hook processes and across crashes.

use blake3::Hasher;

/// A 16-byte OpenTelemetry trace id, derived from a harness session id.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TraceId([u8; 16]);

/// An 8-byte OpenTelemetry span id, derived from a session id + logical key.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct SpanId([u8; 8]);

impl TraceId {
    /// One trace per harness session; stable across processes and restarts.
    pub fn from_session(session_id: &str) -> Self {
        let h = blake3::hash(session_id.as_bytes());
        let mut b = [0u8; 16];
        b.copy_from_slice(&h.as_bytes()[..16]);
        Self(b)
    }

    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl SpanId {
    /// Derive a span id from the session and a logical key, e.g. `"session"`,
    /// `"turn:3"`, `"tool:tu_01"`, `"agent:ag_1"`.
    pub fn derive(session_id: &str, logical_key: &str) -> Self {
        let mut h = Hasher::new();
        h.update(session_id.as_bytes());
        h.update(b":");
        h.update(logical_key.as_bytes());
        let mut b = [0u8; 8];
        b.copy_from_slice(&h.finalize().as_bytes()[..8]);
        Self(b)
    }

    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parse an 8-byte (16 hex char) span id. Returns `None` on malformed input.
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 16 {
            return None;
        }
        let mut b = [0u8; 8];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(b))
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_id_is_deterministic_16_bytes() {
        let a = TraceId::from_session("sess-abc");
        let b = TraceId::from_session("sess-abc");
        assert_eq!(a, b);
        assert_eq!(a.to_hex().len(), 32);
        assert_ne!(a, TraceId::from_session("sess-xyz"));
    }

    #[test]
    fn span_id_parent_independent_of_emit_order() {
        let turn = SpanId::derive("sess", "turn:3");
        let tool = SpanId::derive("sess", "tool:tu_01");
        assert_eq!(tool.to_hex().len(), 16);
        assert_ne!(turn, tool);
        assert_eq!(SpanId::derive("sess", "turn:3"), turn);
    }
}
