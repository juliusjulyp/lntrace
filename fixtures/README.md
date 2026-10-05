# Fixtures

Captured CLN notification data from the Docker regtest lab.

## Topology

4-node diamond with CLN v26.06.8 and xpay:

```
A ── B ── C
 \       /
  ── D ──
```

All channels are 1,000,000 sat. D's fees are raised to 5000 msat base + 1000 ppm (via `setchannel`), making the B path ~100x cheaper so xpay always tries B first when it has liquidity.

## Scenarios

Each scenario truncates all node logs before starting, so fixtures contain only that scenario's events.

| Directory | Payment | Outcome |
|---|---|---|
| `4node-success/` | A pays C 50k sat | Single attempt via B succeeds |
| `4node-reroute/` | Drain B→C 900k, then A pays C 50k sat | Attempt 1 fails via B (`temporary_channel_failure`), xpay retries via D |
| `4node-allfail/` | Drain D→C 900k, then A pays C 100k sat | All attempts fail — both paths drained |

Scenario 3 uses 100k sat (not 50k) because xpay's MPP splitting can succeed with 50k across two drained channels (~35k spendable each, ~70k total). 100k exceeds the combined spendable capacity.

## File format

Each file is newline-delimited JSON (JSONL). Each line:

```json
{"topic":"pay_part_start","payload":{...},"ts_ms":1727856000000,"node_id":"03af...","alias":"node-B"}
```

- `topic`: CLN notification name or `listpeerchannels` (polled snapshot)
- `payload`: raw CLN notification JSON
- `ts_ms`: unix milliseconds
- `node_id`: reporting node's compressed public key
- `alias`: CLN node alias

## Legacy fixtures

The top-level `node-{A,B,C}-raw.jsonl` files are from the original 3-node lab (A→B→C, no D). They serve as the regression baseline for `golden_correlator_traces`.

## Recapture

```bash
cd lab
docker compose up -d --build
bash fund-and-pay.sh
docker compose down -v
```

The script opens channels, sets D's fees, waits for gossip propagation, then runs all three scenarios sequentially. Fixture files are copied out of the containers into `fixtures/`.
