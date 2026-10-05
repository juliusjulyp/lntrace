use crate::capability::Capabilities;
use crate::envelope::Envelope;
use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

/// Adapter interface. Each Lightning implementation provides one.
///
/// Adapters translate implementation-specific events into [`Envelope`]
/// values carrying [`TraceEvent`](crate::event::TraceEvent) payloads.
/// They assign monotonic per-node sequence numbers and declare their
/// [`Capabilities`] so the correlator and UI know what data to expect.
///
/// Adapters are observe-only: they never call payment, channel, or
/// wallet methods on the node.
#[async_trait]
pub trait EventSource: Send + Sync + 'static {
    /// Declare what this adapter can provide.
    fn capabilities(&self) -> Capabilities;

    /// Stream normalized trace events from the node.
    ///
    /// Each [`Envelope`] carries a per-node sequence number assigned
    /// by the adapter. The stream runs until the adapter disconnects
    /// or the node shuts down.
    async fn subscribe(&self) -> anyhow::Result<Pin<Box<dyn Stream<Item = Envelope> + Send>>>;

    /// Poll current channel/balance state.
    ///
    /// Returns `Snapshot` and/or `BalanceUpdate` events reflecting
    /// the node's current state. Called on adapter connect and
    /// periodically thereafter.
    async fn poll_state(&self) -> anyhow::Result<Vec<Envelope>>;
}
