# Design

Architecture, event schema, capability matrix, and detailed roadmap for `lntrace`.

---

## Architecture

```
[LDK nodes] ──────────┐
[CLN plugin] ──────────┤   normalized events
[LND gRPC] ────────────┼──────────────────────> Collector
                       │   (protobuf/JSON over       |
                       │    WebSocket or gRPC)       +-- state store
                       │                             +-- correlator
                       │                             +-- event log (JSONL)
                       │                                    |
                       │                             +------+------+
                       │                             v             v
                       │                        OTel export   Graph UI
                       │                      (Jaeger, Tempo,  (lab /
                       +───────────────────     Grafana)       demo)
```

- **Adapters** run next to each node: in-process for LDK, as a plugin for CLN, and as a gRPC client for LND. They push normalized events to the collector.
- **The collector** stores network state, writes every event to an append-only log, and serves traces via CLI, OpenTelemetry export, and a lightweight graph UI for lab/demo use.
- **The correlator** builds per-payment traces. It is a pure function (`Vec<Envelope> -> Vec<Trace>`) with no I/O, fully testable with fixture data.

---

## Event schema

Every event is wrapped in an Envelope carrying: the observing node ID, a per-node sequence number, the node's wall-clock timestamp, and the collector's receive timestamp. Schema version 1 from day one for forward compatibility.

| Event | Description | Source |
|---|---|---|
| `Snapshot` | Full channel/balance state dump on adapter connect | polled (all) |
| `ChannelPending` | Funding transaction broadcast | all |
| `ChannelReady` | Channel confirmed and usable | all |
| `ChannelClosed` | Channel closed (cooperative, force, or breach) | all |
| `BalanceUpdate` | Local/remote balance change | polled (all) |
| `ForwardEvent` | HTLC forward offered, settled, failed, or local_failed | CLN, LND |
| `PaymentSent` | Outbound payment completed | all |
| `PaymentFailed` | Outbound payment failed | all |
| `PaymentReceived` | Inbound payment settled | all |
| `PaymentPathAttempt` | Individual path/shard dispatched with full route | CLN (`pay_part_start`), LND |
| `PaymentPathResult` | Individual path/shard settled or failed with failing hop | CLN (`pay_part_end`), LND |

---

## Capability matrix

| Capability | LDK Node | Core Lightning | LND |
|---|---|---|---|
| Adapter status | stub | done (sender) | planned |
| Channel lifecycle | yes | yes | yes |
| Forward events (in flight) | no | yes (`forward_event`) | yes (`SubscribeHtlcEvents`) |
| Forward settled / failed | settled only | yes (settled/failed/local_failed) | yes |
| Payment hash on forwards | no (inferred) | yes | via settle preimage |
| Sender: per-path events | no | yes (xpay `pay_part_start`/`pay_part_end`) | yes (`TrackPayments`) |
| Sender: failing hop | no | yes (`failed_node_id`/`failed_short_channel_id`) | yes (`failure_source_index`) |
| Sender: MPP shard detail | no | yes (`payment_hash`, `groupid`, `partid`) | yes (HTLC attempts) |
| In-flight HTLC visibility | no | yes | yes |

---

## Cross-node failure enrichment

This is the key insight that makes `lntrace` more than a log viewer. BOLT 4's `temporary_channel_failure` (code 7) is notoriously overloaded: the sender sees "temporary failure" but can't distinguish insufficient liquidity, too many in-flight HTLCs, HTLC minimum not met, or a channel that is closing.

When the failing node is also instrumented, `lntrace` correlates the sender's generic failure code with the actual cause on the failing node:

- CLN's `forward_event` carries the local failure code and reason on `local_failed` (e.g. "Capacity exceeded")
- LND's link-fail events carry a `failure_detail` (e.g. "INSUFFICIENT_BALANCE")

The trace then shows: "hop B->C failed: `temporary_channel_failure` — actual cause: insufficient liquidity (local balance 45,000 sat, HTLC requested 50,000 sat)."

This enrichment requires the failing node to run CLN or LND. LDK Node intermediates currently emit nothing on a failed forward.

---

## Crate dependency graph

```
lntrace-cli
  -> lntrace-core
  -> lntrace-collector
  -> lntrace-correlator -> lntrace-core
  -> lntrace-explain

lntrace-cln (adapter + recorder)
  -> lntrace-core
  -> lntrace-collector
  -> cln-plugin, cln-rpc

lntrace-ldk (stub)
  -> lntrace-core
  -> lntrace-collector
```

---

## Roadmap

### Stage 2 — Correlator (next)

Build the correlator's `build_trace()` function using the captured fixture data. This is the core of the project — turning raw events into structured payment traces.

- [ ] Implement `build_trace()`: construct hops from `sendpay_success`/`sendpay_failure` events
- [ ] Extract failing hop data (erring_node, erring_channel, failcode) into TracedHop
- [ ] Wire CLI `trace` command to show real hop-by-hop output
- [ ] Add correlator tests using regtest fixtures
- [ ] Integrate BOLT 4 explainer into trace output for failed payments

### Stage 3 — Cross-node correlation

Merge data from multiple instrumented nodes into a single trace. This enables the failure enrichment described above.

- [ ] Multi-node event ingestion (collector accepts events from N adapters)
- [ ] Forward event correlation: match sender's failing hop with intermediate's `forward_event`
- [ ] `temporary_channel_failure` enrichment with actual cause from the failing node
- [ ] Confidence markers on inferred hops (channel adjacency + timing-based matching)
- [ ] Add LDK adapter: channel lifecycle + forward-settled events (intermediate/receiver role)

### Stage 4 — MPP and graph UI

Multi-part payment support and visual debugging.

- [ ] MPP shard decomposition using CLN `pay_part_start`/`pay_part_end`
- [ ] Shard tree display: per-shard paths, which shard failed, `mpp_timeout` detection
- [ ] Lightweight graph UI: nodes, channels, HTLC animation for lab demos
- [ ] Shareable trace file format with pseudonymization

### Stage 5 — LND and OpenTelemetry

- [ ] LND adapter (`routerrpc.SubscribeHtlcEvents` + `routerrpc.TrackPayments`)
- [ ] OpenTelemetry span export (Jaeger / Grafana Tempo compatible)
- [ ] Scenario runner: declarative test scenarios with assertions and CI mode

### Stage 6 — Replay and pathfinding analysis

- [ ] Counterfactual route replay: "what if channel X was excluded?"
- [ ] rust-lightning `Router` wrapper to capture pathfinding decisions and scorer state
- [ ] Stuck HTLC detection and CLTV expiry countdown (CLN + LND)

### Research directions

- **BOLT 12 flow tracing.** Offers add new failure modes (blinded paths, onion messages). Requires rust-lightning-level instrumentation.
- **Channel jamming classification.** Detecting anomalous HTLC hold times is feasible; labeling intent (slow peer vs probe vs jam) is an open research question.
- **LSPS protocol tracing.** JIT channel opens (LSPS1/LSPS2) are handled internally by LDK Node without public events.

---

## Who it's for

- **Primary: developers building on Lightning** — wallets, LSPs, apps — who test multi-hop behaviour on regtest or signet.
- **Also:** LSPs running mixed implementations (e.g. CLN sender with LDK relay nodes) who need cross-implementation debugging.
- **Not:** mainnet node operators who need monitoring dashboards. RTL, ThunderHub and Prometheus exporters serve that use case.

## Scenarios

Declarative scenario files set up networks and drive payments for reproducible testing. The `[network]` section describes topology; `[[step]]` sections describe payment and failure injection.

```toml
# examples/reroute.toml
[network]
nodes    = ["A:cln", "B:ldk", "C:ldk", "D:ldk"]
channels = ["A-B:1_000_000", "B-C:500_000", "C-D:1_000_000", "B-D:200_000"]

[[step]]
action = "pay"
from   = "A"
to     = "D"
amount_sat = 50_000

[step.expect]
outcome = "success"
via     = ["B", "C"]

[[step]]
action  = "drain"
channel = "B-C"
leave_sat = 1_000

[[step]]
action = "pay"
from   = "A"
to     = "D"
amount_sat = 50_000

[step.expect]
outcome = "success"
via     = ["B"]   # reroutes via B-D after B-C is drained
```

The scenario runner is planned for Stage 5.

## Non-goals

- Moving funds, rebalancing or managing nodes.
- Replacing RTL, ThunderHub or Grafana dashboards.
- Showing full payment paths for nodes you don't operate (onion routing prevents this by design).

## Prior art

| Project | What it does | How `lntrace` differs |
|---|---|---|
| [Polar](https://lightningpolar.com/) | One-click regtest networks | Polar sets up networks; `lntrace` traces payments across them. Complementary. |
| [SimLN](https://github.com/bitcoin-dev-project/sim-ln) | Generates realistic payment activity | SimLN drives traffic; `lntrace` debugs it. Complementary. |
| [lnprototest](https://github.com/rustyrussell/lnprototest) | Protocol-level conformance tests | Different layer: wire protocol vs application-level payment flows. |
| RTL, ThunderHub | Single-node management dashboards | `lntrace` correlates across nodes for debugging, not node management. |
| Jaeger, Grafana Tempo | Distributed tracing backends | `lntrace` exports to these. They provide storage and UI; `lntrace` provides Lightning-specific correlation. |

## Security details

- **Observe-only adapters.** The CLN adapter and (planned) LND adapter never call payment, channel, or wallet RPCs. They subscribe to notifications passively.
  - CLN adapter does not use the `htlc_accepted` hook (which would block HTLC processing).
  - LND adapter will connect with `readonly.macaroon` (pending verification for router RPCs).
  - The lab scripts (`fund-and-pay.sh`) do create wallets, fund channels, and send payments — these are test-harness actions, not part of the `lntrace` tool itself.
- **Local-first.** The collector binds to localhost by default. Remote exposure is explicit opt-in.
- **Pseudonymization.** Shared traces replace node keys with consistent aliases and bucket amounts, preserving trace structure while stripping identity.
- **No mainnet scenarios.** Scenario runner targets regtest and signet only.

## Contributing

Adapters are the easiest place to start. Each adapter implements the `EventSource` trait:

```rust
#[async_trait]
pub trait EventSource: Send + Sync + 'static {
    fn capabilities(&self) -> Capabilities;
    async fn subscribe(&self) -> Result<Pin<Box<dyn Stream<Item = TraceEvent> + Send>>>;
    async fn poll_state(&self) -> Result<Vec<TraceEvent>>;
}
```

Declare your capabilities, and add recorded fixture events for conformance tests.
