//! Core data model for gently: deterministic ids, the internal span type, and
//! the OTLP/JSON wire encoding. This crate has no I/O.

pub mod ids;
pub mod otlp;
pub mod span;

pub use ids::{SpanId, TraceId};
pub use otlp::OtlpRequest;
pub use span::{Resource, Span, SpanKind, Status};
