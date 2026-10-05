# lntrace

> Your Lightning node tells you *that* a payment failed. lntrace shows you *where* and *why*.

[![CI](https://github.com/juliusjulyp/lntrace/actions/workflows/ci.yml/badge.svg)](https://github.com/juliusjulyp/lntrace/actions/workflows/ci.yml)

When a multi-hop payment fails, the sender only gets a generic error such as
`temporary_channel_failure`. The real reason sits on another node. lntrace
collects events from every node it runs on, links them into one trace per
payment, and shows the failing hop, the likely cause with real numbers, and
any retries — including what they cost.

![A payment that failed at B→C and succeeded on retry via D](docs/lntrace-reroute.png)
*Attempt 1 failed at node-B → node-C: 35,694 sat available, 50,000 needed. The retry via node-D succeeded and cost ≈55 sat more.*

Currently supports CLN, with LDK and LND adapters planned.

---

## Try it in 30 seconds (no nodes needed)

```bash
git clone https://github.com/juliusjulyp/lntrace
cd lntrace
cargo build
cargo run -p lntrace-cli -- ui --log fixtures/4node-reroute/
# open http://localhost:8080
```

This runs entirely from recorded data — no Docker, no nodes. Click a trace in the sidebar, then Play to step through the payment.

---

## How it works

```
CLN node A ──→ lntrace-cln plugin ──→ events.jsonl ──┐
CLN node B ──→ lntrace-cln plugin ──→ events.jsonl ──┤
CLN node D ──→ lntrace-cln plugin ──→ events.jsonl ──┤
                                                      ▼
                                              correlator
                                                      │
                                              traces + graph
                                                      │
                                          ┌───────────┴───────────┐
                                          ▼                       ▼
                                    CLI (replay)            UI (browser)
```

1. **Adapters** run as plugins on each node, subscribing to notifications passively and writing JSONL event logs. They never call payment, channel, or wallet RPCs.
2. **The correlator** pairs each path attempt with its result by `(groupid, partid)`, merges forward events across nodes, and infers the actual cause of failures from channel state snapshots.
3. **The CLI and UI** consume the correlated traces. The replay UI renders the payment graph with route focus, step/play animation, and a summary panel showing the cost of retries.

See [docs/design.md](docs/design.md) for the event schema, capability matrix, and full architecture.

---

## Use with your own CLN nodes

Build the plugin and point your node at it:

```bash
cargo build -p lntrace-cln --release
```

Then either start lightningd with the plugin:

```bash
lightningd --plugin=/path/to/target/release/lntrace-cln
```

Or load it at runtime:

```bash
lightning-cli plugin start /path/to/target/release/lntrace-cln
```

The plugin writes `events.jsonl` in the node's lightning directory. To view traces from one or more nodes, point `ui` at a directory containing the log files:

```bash
cargo run -p lntrace-cli -- ui --log /path/to/logs/
```

---

## CLI commands

```bash
# decode a BOLT 4 failure code
cargo run -p lntrace-cli -- explain 0x1007

# list events from a fixture directory
cargo run -p lntrace-cli -- events --log fixtures/4node-reroute/

# show all payment traces
cargo run -p lntrace-cli -- replay fixtures/4node-reroute/

# trace a specific payment
cargo run -p lntrace-cli -- trace <payment_hash> --log fixtures/4node-reroute/

# launch the graph UI
cargo run -p lntrace-cli -- ui --log fixtures/4node-reroute/
```

---

## Screenshots

| Overview | Success trace |
|---|---|
| ![Network overview](docs/lntrace-overview.png) | ![Success trace](docs/lntrace-success.png) |

---

## Status

All tests pass. The replay UI, CLN adapter, correlator, and CLI are functional.

| Component | Status | Description |
|---|---|---|
| `lntrace` library | done | Types, JSONL collector, correlator, graph builder, BOLT 4 decoder |
| CLI | done | `events`, `trace`, `replay`, `explain`, `ui` subcommands |
| Replay UI | done | Cytoscape.js graph with fcose layout, route focus, search/filter, step/play animation, summary panel |
| CLN adapter | done | Typed xpay deserialization, listpeerchannels polling, recorder plugin |
| Correlator | done | Per-shard attempt pairing, direction-aware hop matching, cause inference from channel snapshots |
| Docker lab | done | 4-node diamond CLN v26.06.8 lab with success, reroute, and total-failure scenarios |

---

## Roadmap

| Milestone | Focus | Status |
|---|---|---|
| v0.1 | Core trace pipeline: CLN adapter, correlator, replay UI. Remaining: live mode (`ui --follow`) | in progress |
| v0.2 | LDK adapter, MPP shard trees, OTel export, shareable traces | planned |
| v0.3 | LND adapter, scenario runner | planned |
| Future | Counterfactual replay, BOLT 12 tracing, channel jamming detection, LSPS | research |

---

## Known limitations

- **Onion routing limits visibility.** You only see hops on nodes you run lntrace on. If two of four hops are uninstrumented, the trace has gaps.
- **Causes are inferred, not reported.** When a hop fails with `temporary_channel_failure`, lntrace checks the failing node's channel state and infers the most likely cause (insufficient liquidity, HTLC minimum, too many HTLCs). The inference is usually right, but it's not authoritative.
- **CLN only (for now).** LDK and LND adapters are on the roadmap. LDK Node doesn't yet expose the events needed for full trace reconstruction.

---

## How it compares

| Tool | Focus |
|---|---|
| **Polar** | Visual regtest node management. Launches nodes; doesn't trace payments across them. |
| **SimLN** | Payment traffic simulator. Generates load; doesn't diagnose failures. |
| **RTL / ThunderHub** | Node dashboards. Show your own node's payments; don't correlate across hops. |
| **lntrace** | Cross-node payment tracing. Shows the full path, the failing hop, and the inferred cause. |

---

## The lab

A 4-node diamond topology for development and testing:

```
A ── B ── C
 \       /
  ── D ──
```

```bash
cd lab
docker compose up -d --build
bash fund-and-pay.sh          # opens channels, runs 3 scenarios, captures fixtures
docker compose down -v
```

D's fees are raised so xpay prefers B when it has liquidity. Three scenarios are captured:

1. **Success** (`fixtures/4node-success/`): A pays C via B.
2. **Reroute** (`fixtures/4node-reroute/`): B→C drained, A pays C. Attempt 1 fails via B, xpay retries via D.
3. **Total failure** (`fixtures/4node-allfail/`): Both B→C and D→C drained. All attempts fail with inferred causes.

---

## Project structure

```
lntrace/
  crates/
    lntrace/               # library: types, collector, correlator, graph, BOLT 4 decoder
    lntrace-cli/           # CLI binary + graph UI server
  adapters/
    cln/                   # CLN plugin + recorder
  lab/                     # Docker regtest lab
  fixtures/                # recorded CLN notification data from regtest
```

---

## Security and privacy

- **Observe-only adapters.** The CLN adapter subscribes to notifications passively — no payment, channel, or wallet RPCs. Lab scripts (`fund-and-pay.sh`) do send payments, but those are test-harness actions, not part of the tool.
- **Local-first.** The UI server binds to localhost by default.

---

## Contributing

The adapter interface is the `EventSource` trait in `crates/lntrace/src/source.rs`. To add support for a new implementation (LDK, LND), implement the three methods — `capabilities`, `subscribe`, `poll_state` — and write a binary that runs as a plugin or sidecar for that node.

---

## License

Dual-licensed under MIT or Apache-2.0, at your option.
