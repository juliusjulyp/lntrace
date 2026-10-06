#!/usr/bin/env bash
# 4-node regtest lab: fund channels, run 3 payment scenarios, capture fixtures.
#
# Topology:
#   A ── B ── C
#    \       /
#     ── D ──
#
# Channels: A→B (1M sat), B→C (1M sat), A→D (1M sat), D→C (1M sat)
# D's fees raised to 5000 base + 1000ppm so xpay prefers B route.
#
# Scenario 1 (success):      A pays C 50k sat → routes via B (cheapest)
# Scenario 2 (reroute):      Drain B→C, A pays C → fails via B, retries via D
# Scenario 3 (total fail):   Also drain D→C, A pays C → all paths fail
#
# Run after: docker compose up -d --build
set -euo pipefail

BCLI="docker exec lntrace-bitcoind bitcoin-cli -regtest -rpcuser=lntrace -rpcpassword=lntrace"
LCLI_A="docker exec lntrace-cln-a lightning-cli --network=regtest"
LCLI_B="docker exec lntrace-cln-b lightning-cli --network=regtest"
LCLI_C="docker exec lntrace-cln-c lightning-cli --network=regtest"
LCLI_D="docker exec lntrace-cln-d lightning-cli --network=regtest"

# Recorder output lives in the shared /logs bind-mount (see docker-compose.yml).
LOG_DIR="$(cd "$(dirname "$0")" && pwd)/logs"

# ── Helpers ──────────────────────────────────────────────────────────

wait_for_node() {
    local name="$1" cli="$2"
    for i in $(seq 1 30); do
        if $cli getinfo > /dev/null 2>&1; then return 0; fi
        echo "  waiting for $name... ($i/30)"
        sleep 2
    done
    echo "ERROR: $name did not start" >&2; exit 1
}

mine() {
    local n="$1"
    $BCLI generatetoaddress "$n" "$($BCLI getnewaddress)" > /dev/null
}

truncate_raw_logs() {
    mkdir -p "$LOG_DIR"
    for node in A B C D; do
        truncate -s 0 "$LOG_DIR/node-$node-raw.jsonl" 2>/dev/null || true
    done
    echo "  Truncated raw logs on all nodes"
    sleep 1
}

copy_scenario_fixtures() {
    local scenario_dir="$1"
    mkdir -p "$scenario_dir"
    for node in A B C D; do
        SRC="$LOG_DIR/node-$node-raw.jsonl"
        if [ -f "$SRC" ]; then
            cp "$SRC" "$scenario_dir/node-$node-raw.jsonl"
            echo "  Copied node $node"
        else
            echo "  WARNING: no raw log from node $node"
        fi
    done
}

FIXTURE_DIR="$(cd "$(dirname "$0")/.." && pwd)/fixtures"

# ── 0. Wait for all nodes ────────────────────────────────────────────
echo "=== Waiting for nodes ==="
wait_for_node "A" "$LCLI_A"
wait_for_node "B" "$LCLI_B"
wait_for_node "C" "$LCLI_C"
wait_for_node "D" "$LCLI_D"

ID_A=$($LCLI_A getinfo | jq -r '.id')
ID_B=$($LCLI_B getinfo | jq -r '.id')
ID_C=$($LCLI_C getinfo | jq -r '.id')
ID_D=$($LCLI_D getinfo | jq -r '.id')
echo "Node A: $ID_A"
echo "Node B: $ID_B"
echo "Node C: $ID_C"
echo "Node D: $ID_D"

# ── 1. Mine initial blocks and fund nodes ────────────────────────────
echo ""
echo "=== Setting up blockchain ==="
$BCLI createwallet "default" 2>/dev/null || $BCLI loadwallet "default" 2>/dev/null || true

BLOCKS=$($BCLI getblockcount)
if [ "$BLOCKS" -lt 101 ]; then
    ADDR=$($BCLI getnewaddress)
    $BCLI generatetoaddress 101 "$ADDR" > /dev/null
    echo "Mined 101 blocks"
fi

# Wait for all nodes to sync
echo "Syncing nodes to chain..."
for i in $(seq 1 60); do
    HA=$($LCLI_A getinfo | jq -r '.blockheight')
    HB=$($LCLI_B getinfo | jq -r '.blockheight')
    HC=$($LCLI_C getinfo | jq -r '.blockheight')
    HD=$($LCLI_D getinfo | jq -r '.blockheight')
    CHAIN=$($BCLI getblockcount)
    if [ "$HA" -ge "$CHAIN" ] && [ "$HB" -ge "$CHAIN" ] && [ "$HC" -ge "$CHAIN" ] && [ "$HD" -ge "$CHAIN" ]; then
        echo "  All nodes at block $CHAIN"
        break
    fi
    sleep 2
done

# Fund A (2 BTC — opens 2 channels), B (1 BTC), D (1 BTC)
echo "Funding nodes..."
ADDR_A=$($LCLI_A newaddr | jq -r '.bech32')
ADDR_B=$($LCLI_B newaddr | jq -r '.bech32')
ADDR_D=$($LCLI_D newaddr | jq -r '.bech32')
$BCLI sendtoaddress "$ADDR_A" 2.0 > /dev/null
$BCLI sendtoaddress "$ADDR_B" 1.0 > /dev/null
$BCLI sendtoaddress "$ADDR_D" 1.0 > /dev/null
mine 6
echo "  Sent 2 BTC to A, 1 BTC each to B and D"

# Wait for funds to appear
for node_label in "A:$LCLI_A" "B:$LCLI_B" "D:$LCLI_D"; do
    IFS=: read -r name cli <<< "$node_label"
    for i in $(seq 1 30); do
        FUNDS=$($cli listfunds | jq '.outputs | length')
        if [ "$FUNDS" -gt 0 ]; then break; fi
        sleep 2
    done
done

# ── 2. Open channels: A→B, B→C, A→D, D→C ────────────────────────────
echo ""
echo "=== Opening channels ==="

echo "Connecting A → B..."
$LCLI_A connect "$ID_B@lntrace-cln-b:9735" > /dev/null 2>&1 || true
echo "Opening A→B channel (1,000,000 sat)..."
$LCLI_A fundchannel "$ID_B" 1000000 | jq '{txid, channel_id}'

echo "Connecting B → C..."
$LCLI_B connect "$ID_C@lntrace-cln-c:9735" > /dev/null 2>&1 || true
echo "Opening B→C channel (1,000,000 sat)..."
$LCLI_B fundchannel "$ID_C" 1000000 | jq '{txid, channel_id}'

# Mine blocks to confirm change outputs before opening more channels from A and D
mine 3
echo "Mined 3 blocks to confirm change outputs"
sleep 5  # let nodes process blocks and update wallet

echo "Connecting A → D..."
$LCLI_A connect "$ID_D@lntrace-cln-d:9735" > /dev/null 2>&1 || true
echo "Opening A→D channel (1,000,000 sat)..."
for attempt in $(seq 1 10); do
    RESULT=$($LCLI_A fundchannel "$ID_D" 1000000 2>&1) && break
    echo "  Retry $attempt: $(echo "$RESULT" | jq -r '.message // "unknown"' 2>/dev/null)"
    sleep 3
done
echo "$RESULT" | jq '{txid, channel_id}'

echo "Connecting D → C..."
$LCLI_D connect "$ID_C@lntrace-cln-c:9735" > /dev/null 2>&1 || true
echo "Opening D→C channel (1,000,000 sat)..."
for attempt in $(seq 1 10); do
    RESULT=$($LCLI_D fundchannel "$ID_C" 1000000 2>&1) && break
    echo "  Retry $attempt: $(echo "$RESULT" | jq -r '.message // "unknown"' 2>/dev/null)"
    sleep 3
done
echo "$RESULT" | jq '{txid, channel_id}'

# Confirm channels
mine 6
echo "Mined 6 blocks to confirm channels"

# Wait for all four channels to be CHANNELD_NORMAL
echo "Waiting for channels to become active..."
for i in $(seq 1 60); do
    STATE_AB=$($LCLI_A listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_B"'")] | .[0].state // "none"')
    STATE_BC=$($LCLI_B listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_C"'")] | .[0].state // "none"')
    STATE_AD=$($LCLI_A listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_D"'")] | .[0].state // "none"')
    STATE_DC=$($LCLI_D listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_C"'")] | .[0].state // "none"')
    if [ "$STATE_AB" = "CHANNELD_NORMAL" ] && [ "$STATE_BC" = "CHANNELD_NORMAL" ] && \
       [ "$STATE_AD" = "CHANNELD_NORMAL" ] && [ "$STATE_DC" = "CHANNELD_NORMAL" ]; then
        echo "  All four channels active"
        break
    fi
    echo "  A→B:$STATE_AB B→C:$STATE_BC A→D:$STATE_AD D→C:$STATE_DC ($i/60)"
    sleep 2
done

# ── 3. Raise D's fees ────────────────────────────────────────────────
echo ""
echo "=== Setting D's fees high (5000 base + 1000ppm) ==="
SCID_DC=$($LCLI_D listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_C"'")] | .[0].short_channel_id // "none"')
SCID_DA=$($LCLI_D listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_A"'")] | .[0].short_channel_id // "none"')

echo "D→C scid: $SCID_DC"
echo "D→A scid: $SCID_DA"

$LCLI_D setchannel "$SCID_DC" 5000 1000 | jq '{short_channel_id, fee_base_msat, fee_proportional_millionths}'
$LCLI_D setchannel "$SCID_DA" 5000 1000 | jq '{short_channel_id, fee_base_msat, fee_proportional_millionths}'

# ── 4. Wait for gossip ──────────────────────────────────────────────
echo ""
echo "=== Waiting for gossip ==="
# Mine more blocks so channels get announced (need 6 confirmations)
mine 6

SCID_AB=$($LCLI_A listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_B"'")] | .[0].short_channel_id // "none"')
SCID_BC=$($LCLI_B listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_C"'")] | .[0].short_channel_id // "none"')
SCID_AD=$($LCLI_A listpeerchannels | jq -r '[.channels[] | select(.peer_id == "'"$ID_D"'")] | .[0].short_channel_id // "none"')

echo "A→B scid: $SCID_AB"
echo "B→C scid: $SCID_BC"
echo "A→D scid: $SCID_AD"
echo "D→C scid: $SCID_DC"

for i in $(seq 1 90); do
    TOTAL=0
    for scid in "$SCID_AB" "$SCID_BC" "$SCID_AD" "$SCID_DC"; do
        DIRS=$($LCLI_A listchannels "$scid" 2>/dev/null | jq '.channels | length')
        TOTAL=$((TOTAL + DIRS))
    done
    if [ "$TOTAL" -ge 8 ]; then
        echo "  A sees all 4 channels in both directions ($TOTAL/8)"
        break
    fi
    echo "  A sees $TOTAL/8 channel entries ($i/90)"
    sleep 3
done

# Extra wait for D's fee update to propagate through gossip
echo "Waiting for D's fee gossip to propagate..."
sleep 10
mine 1
sleep 5

# Verify A sees D's high fees
D_FEE=$($LCLI_A listchannels "$SCID_DC" | jq '[.channels[] | select(.source == "'"$ID_D"'")] | .[0].fee_per_millionth // 0')
echo "A sees D→C fee_per_millionth: $D_FEE (should be 1000)"

# ── 5. Scenario 1: Success ──────────────────────────────────────────
echo ""
echo "=== Scenario 1: Success (A pays C 50k sat, expect route via B) ==="
truncate_raw_logs
sleep 3  # let initial listpeerchannels snapshots flush

INVOICE_1=$($LCLI_C invoice 50000000 "success-$(date +%s)" "4node success" | jq -r '.bolt11')
echo "Invoice: ${INVOICE_1:0:50}..."

PAY_1=$($LCLI_A xpay "$INVOICE_1" 2>&1) || true
echo "Result:"
echo "$PAY_1" | jq '{payment_hash, status}' 2>/dev/null || echo "$PAY_1"

sleep 3
copy_scenario_fixtures "$FIXTURE_DIR/4node-success"

# ── 6. Scenario 2: Reroute ──────────────────────────────────────────
echo ""
echo "=== Scenario 2: Reroute (drain B→C, then A pays C) ==="
truncate_raw_logs
sleep 3

# Drain B→C (B pays C directly to empty B's outbound liquidity to C)
echo "Draining B→C channel..."
DRAIN1_INV=$($LCLI_C invoice 900000000 "drain-bc-$(date +%s)" "drain B→C" | jq -r '.bolt11')
DRAIN1_RESULT=$($LCLI_B xpay "$DRAIN1_INV" 2>&1) || true
echo "Drain B→C:"
echo "$DRAIN1_RESULT" | jq '{payment_hash, status}' 2>/dev/null || echo "$DRAIN1_RESULT"
sleep 2

# Check B→C liquidity after drain
echo ""
echo "B→C liquidity after drain:"
$LCLI_B listpeerchannels | jq '[.channels[] | select(.peer_id == "'"$ID_C"'")] | .[0] | {spendable_msat, receivable_msat}'

# A pays C — xpay should try B first (cheaper), fail, then retry via D
echo ""
echo "A pays C 50k sat (expect fail via B, retry via D)..."
INVOICE_2=$($LCLI_C invoice 50000000 "reroute-$(date +%s)" "4node reroute" | jq -r '.bolt11')
PAY_2=$($LCLI_A xpay "$INVOICE_2" 2>&1) || true
echo "Result:"
echo "$PAY_2" | jq '{payment_hash, status}' 2>/dev/null || echo "$PAY_2"

sleep 3
copy_scenario_fixtures "$FIXTURE_DIR/4node-reroute"

# ── 7. Scenario 3: Total failure ────────────────────────────────────
echo ""
echo "=== Scenario 3: Total failure (drain D→C too, then A pays C) ==="
truncate_raw_logs
sleep 3

# Drain D→C
echo "Draining D→C channel..."
DRAIN2_INV=$($LCLI_C invoice 900000000 "drain-dc-$(date +%s)" "drain D→C" | jq -r '.bolt11')
DRAIN2_RESULT=$($LCLI_D xpay "$DRAIN2_INV" 2>&1) || true
echo "Drain D→C:"
echo "$DRAIN2_RESULT" | jq '{payment_hash, status}' 2>/dev/null || echo "$DRAIN2_RESULT"
sleep 2

# Check D→C liquidity after drain
echo ""
echo "D→C liquidity after drain:"
$LCLI_D listpeerchannels | jq '[.channels[] | select(.peer_id == "'"$ID_C"'")] | .[0] | {spendable_msat, receivable_msat}'

# A pays C — both paths fail (100k sat exceeds combined spendable on both drained channels)
echo ""
echo "A pays C 100k sat (expect total failure)..."
INVOICE_3=$($LCLI_C invoice 100000000 "allfail-$(date +%s)" "4node total failure" | jq -r '.bolt11')
PAY_3=$($LCLI_A xpay "$INVOICE_3" 2>&1) || true
echo "Result:"
echo "$PAY_3" | jq '.' 2>/dev/null || echo "$PAY_3"

sleep 3
copy_scenario_fixtures "$FIXTURE_DIR/4node-allfail"

# ── 8. Verification ─────────────────────────────────────────────────
echo ""
echo "=== Verification ==="
for scenario in 4node-success 4node-reroute 4node-allfail; do
    echo ""
    echo "--- $scenario ---"
    for node in A B C D; do
        f="$FIXTURE_DIR/$scenario/node-$node-raw.jsonl"
        if [ -f "$f" ]; then
            LINES=$(wc -l < "$f")
            TOPICS=$(grep -oP '"topic":"[^"]*"' "$f" 2>/dev/null | sort | uniq -c | sort -rn | head -5)
            echo "  node-$node: $LINES lines"
            echo "$TOPICS" | sed 's/^/    /'
        else
            echo "  node-$node: MISSING"
        fi
    done
done

echo ""
echo "=== Done ==="
echo "Fixtures saved to:"
echo "  $FIXTURE_DIR/4node-success/"
echo "  $FIXTURE_DIR/4node-reroute/"
echo "  $FIXTURE_DIR/4node-allfail/"
echo ""
echo "Run golden tests:"
echo "  cargo test -p lntrace-cln -- --ignored"
echo "  cargo insta review -p lntrace-cln"
