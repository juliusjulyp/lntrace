use crate::capability::Capabilities;
use crate::envelope::Envelope;
use crate::event::ChannelSnapshot;

/// Adapter interface. Each Lightning implementation provides one.
///
/// Adapters translate implementation-specific events into [`Envelope`]
/// values carrying [`TraceEvent`](crate::event::TraceEvent) payloads.
/// They assign monotonic per-node sequence numbers and declare their
/// [`Capabilities`] so the correlator and UI know what data to expect.
///
/// Adapters are observe-only: they use read-only RPCs (e.g.
/// `getinfo`, `listpeerchannels`) but never initiate payments or
/// modify channels or wallets.
///
/// # Dispatch
///
/// This trait uses RPITIT (return-position `impl Trait` in trait) for
/// its async methods. That means it **cannot be used as a trait
/// object** (`Box<dyn EventSource>` won't compile). When multiple
/// adapters coexist (CLN + LDK + LND), use an enum over the concrete
/// types and delegate each method. This avoids the `async-trait`
/// allocation overhead while keeping dispatch straightforward.
pub trait EventSource: Send + Sync + 'static {
    /// Declare what this adapter can provide.
    fn capabilities(&self) -> Capabilities;

    /// Stream normalized trace events from the node.
    ///
    /// Each [`Envelope`] carries a per-node sequence number assigned
    /// by the adapter. The stream runs until the adapter disconnects
    /// or the node shuts down.
    fn subscribe(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<tokio::sync::mpsc::Receiver<Envelope>>> + Send;

    /// Poll current channel state.
    ///
    /// Returns a snapshot of all channels reflecting the node's current
    /// state. Called on adapter connect and periodically thereafter.
    fn poll_state(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<ChannelSnapshot>>> + Send;
}
