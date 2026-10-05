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

This is the key insight that makes `lntrace` more than a log viewer. BOLT 4's `temporary_channel_failure` (code 0x1007) is notoriously overloaded: the sender sees "temporary failure" but can't distinguish insufficient liquidity, too many in-flight HTLCs, HTLC minimum not met, or a channel that is closing.

**What forward_event gives us.** CLN's `forward_event` on `local_failed` carries a `failreason` string, but in practice it's just the same generic wire code (e.g. `WIRE_TEMPORARY_CHANNEL_FAILURE`). This confirms *which* hop failed and that the intermediate node saw the failure locally, but does not reveal the actual cause.

**Where real enrichment comes from.** To determine *why* the forward failed, `lntrace` polls `listpeerchannels` on the failing node to get channel state snapshots: `spendable_msat`, `receivable_msat`, `htlc_maximum_msat`, channel status. When the correlator sees a `temporary_channel_failure` and the channel snapshot shows `spendable_msat < htlc_amount`, it can report: "hop B->C failed: `temporary_channel_failure` — actual cause: insufficient liquidity (spendable 100,000 msat, HTLC requested 50,000,000 msat)."

- **Stage 1 (done):** Forward_event merge — the correlator matches the sender's generic error with the intermediate node's `local_failed` forward, confirming the failing hop.
- **Stage 2 (planned):** `listpeerchannels` polling via `poll_state()` — the CLN adapter periodically snapshots channel state. Snapshots are attached to traces as `Inferred` confidence.
- LND's link-fail events carry a `failure_detail` (e.g. "INSUFFICIENT_BALANCE") which provides richer data than CLN's `forward_event`.

This enrichment requires the failing node to be instrumented. LDK Node intermediates currently emit nothing on a failed forward.

---

## Crate dependency graph

```
lntrace-cli (binary)
  -> lntrace

lntrace-cln (adapter + recorder)
  -> lntrace
  -> cln-plugin 0.7, cln-rpc 0.7, sha2
```

The `lntrace` library crate contains: core types, collector, correlator, and BOLT 4 decoder as modules. The correlator is `pub(crate)` with its public surface re-exported at the crate root.

---

## Roadmap

### v0.1 — Core trace pipeline (in progress)

The minimum viable tool: capture events across a multi-hop network, correlate them into traces, enrich failures with data from the failing node, and display the result in a graph UI.

- [x] Upgrade CLN lab to v26.06.8 with xpay
- [x] 3-node lab (A→B→C) with drained-channel failure scenario
- [x] Recapture fixtures with the recorder (one file per node)
- [x] CLN adapter: typed xpay deserialization (`cln-rpc 0.7`), `sha256(preimage)` for `invoice_payment`
- [x] `build_trace()` with hop reconstruction and cross-node forward_event merge
- [x] Per-shard attempt pairing by `(groupid, partid)` — retries produce separate attempts, not merged routes
- [x] Failing hop match on scid + direction + node_id verification
- [x] Cause rules module: infer actual cause of `temporary_channel_failure` from channel snapshots
- [x] `listpeerchannels` polling via `poll_state()` for liquidity enrichment — periodic + on-demand after `local_failed`
- [x] 32 tests: 19 library (correlator + cause + BOLT 4) + 12 adapter (translate + cross-node hash match) + 1 golden (insta)
- [ ] Recapture fixtures with polling-enabled recorder for enrichment golden test
- [ ] Graph UI: `lntrace ui --log` (replay) and `lntrace ui --follow` (live)
- [ ] Demo script and recording

**Done when:** a drained-channel payment in the live lab shows up in the graph with the failing hop highlighted and the real cause in the side panel, and the same flow replays identically from the recorded log.

### v0.2 — Breadth

- [ ] LDK adapter (intermediate/receiver role): channel lifecycle + forward-settled events
- [ ] MPP shard decomposition using `pay_part_start`/`pay_part_end`
- [ ] Shard tree display: per-shard paths, which shard failed, `mpp_timeout` detection
- [ ] OpenTelemetry span export (Jaeger / Grafana Tempo compatible)
- [ ] Shareable trace file format with pseudonymization

### v0.3 — LND and scenario runner

- [ ] LND adapter (`routerrpc.SubscribeHtlcEvents` + `routerrpc.TrackPayments`)
- [ ] Scenario runner: declarative test scenarios with assertions and CI mode

### Future

- Counterfactual route replay: "what if channel X was excluded?"
- rust-lightning `Router` wrapper to capture pathfinding decisions and scorer state
- Stuck HTLC detection and CLTV expiry countdown (CLN + LND)
- BOLT 12 flow tracing (blinded paths, onion messages)
- Channel jamming classification (anomalous HTLC hold times)
- LSPS protocol tracing (JIT channel opens via LSPS1/LSPS2)

### Anytime

- Open ldk-node upstream issue for per-path payment events (ecosystem engagement)

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

The scenario runner is planned for v0.3.

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
