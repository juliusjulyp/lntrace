# lntrace

[![CI](https://github.com/juliusjulyp/lntrace/actions/workflows/ci.yml/badge.svg)](https://github.com/juliusjulyp/lntrace/actions/workflows/ci.yml)



`lntrace` is a payment debugging tool for the Bitcoin Lightning Network, written in Rust. It connects to nodes running different implementations (CLN, LDK, LND), stitches their events into a single trace per payment, and tells you exactly which hop failed and why — including multi-part payments where shards take different paths.

---

## Current status

Stages 0-1 are complete. The core crates compile and pass tests, the CLN adapter captures real notification data from a regtest lab, and the CLI can inspect events and decode failure codes.

| Component | Status | Description |
|---|---|---|
| Core types | done | TraceEvent (11 variants), Envelope, Capabilities, EventSource trait |
| Collector | done | Async JSONL writer, reader, and tailer |
| BOLT 4 explainer | done | 23 BOLT 4 failure codes with plain-language explanation and guidance |
| CLI | done | `events`, `trace`, `replay`, `explain` subcommands |
| CLN adapter | done | Translates CLN notifications to TraceEvent (via `sendpay_*`; xpay `pay_part_*` requires CLN >= v25.08) |
| Docker lab | done | Bitcoin Core + 2 CLN nodes, automated channel setup and payments |
| Correlator | partial | Groups events by payment_hash; hop construction is Stage 2 |
| LDK adapter | stub | Struct and trait skeleton |

### Known limitations

- **2-node lab.** Sender and receiver with a direct channel — no intermediate hops yet.
- **CLN v25.02 / legacy `pay` only.** xpay notifications (`pay_part_start`/`pay_part_end`) require CLN >= v25.08.
- **Correlator hops are stubbed.** Events are grouped by payment hash but hop-by-hop paths aren't reconstructed yet.
- **No LDK or LND adapters.** LDK is a skeleton; LND is not started.

---

## Getting started

### Build

```bash
git clone https://github.com/juliusjulyp/lntrace
cd lntrace
cargo build
cargo test --all
```

### Try the CLI

```bash
# decode a BOLT 4 failure code
cargo run -p lntrace-cli -- explain 0x1007

# list events from the regtest fixture
cargo run -p lntrace-cli -- events --log fixtures/cln-regtest-envelope.jsonl

# show all payment traces
cargo run -p lntrace-cli -- replay fixtures/cln-regtest-envelope.jsonl

# trace a specific payment
cargo run -p lntrace-cli -- trace <payment_hash> --log fixtures/cln-regtest-envelope.jsonl
```

### Run the Docker regtest lab

```bash
cd lab
docker compose up -d --build
bash fund-and-pay.sh          # opens channel, sends payments, captures fixtures
docker compose down -v         # clean teardown
```

The lab uses CLN v25.02. Fixtures are written to `fixtures/cln-regtest-raw.jsonl` (7 events: channel opens, successful payments, and a `WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS` failure).

---

## Project structure

```
lntrace/
  crates/
    lntrace-core/          # types, events, envelope, capabilities, EventSource trait
    lntrace-collector/     # JSONL log writer, reader, tailer
    lntrace-correlator/    # payment trace builder (partial)
    lntrace-explain/       # BOLT 4 failure code decoder
    lntrace-cli/           # CLI binary
  adapters/
    cln/                   # CLN plugin (recorder + adapter)
    ldk/                   # LDK Node adapter (stub)
  lab/                     # Docker regtest lab
  fixtures/                # real CLN notification data from regtest
```

---

## Roadmap

| Stage | Focus | Status |
|---|---|---|
| 0-1 | Core types, CLN adapter, CLI, regtest lab | done |
| 2 | Correlator — `build_trace()` with real hop reconstruction | next |
| 3 | Cross-node correlation — merge events from multiple nodes | planned |
| 4 | MPP shard decomposition and graph UI | planned |
| 5 | LND adapter, OpenTelemetry export, scenario runner | planned |
| 6 | Counterfactual replay, pathfinding analysis | planned |

See [docs/design.md](docs/design.md) for architecture details, event schema, capability matrix, and the full roadmap with checklists.

---

## Security and privacy

- **Observe-only adapters.** The CLN adapter subscribes to notifications passively — no payment, channel, or wallet RPCs. Lab scripts (`fund-and-pay.sh`) do send payments, but those are test-harness actions, not part of the tool.
- **Local-first.** The collector binds to localhost by default.
- **No mainnet scenarios.** Scenario runner targets regtest and signet only.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
