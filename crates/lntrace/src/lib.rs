pub mod capability;
pub mod cause;
pub mod collector;
pub mod envelope;
pub mod event;
pub mod explain;
pub mod format;
pub mod graph;
pub mod source;
pub mod types;

// Correlator is pub(crate): callers use the re-exported public surface below.
// Keeps the module easy to split into its own crate later.
pub(crate) mod correlator;

// Re-export core types.
pub use capability::Capabilities;
pub use envelope::{now_ms, Envelope, SCHEMA_VERSION};
pub use event::{ChannelSnapshot, TraceEvent};
pub use source::EventSource;
pub use types::*;

// Re-export collector.
pub use collector::{read_log, tail_log, LogHandle, LogWriter};

// Re-export correlator public surface.
pub use correlator::{
    correlate, Attempt, AttemptOutcome, FailureInfo, HopFailure, Trace, TracedHop,
};

// Re-export explain.
pub use explain::{explain, format_failure, FailureExplanation};

// Re-export format.
pub use format::format_sat_fee;

// Re-export cause inference.
pub use cause::{infer_cause, CauseRule, InferredCause};

// Re-export graph builder.
pub use graph::{
    build_graph, build_trace_list, GraphChannel, GraphNode, GraphResponse, TraceListEntry,
};
