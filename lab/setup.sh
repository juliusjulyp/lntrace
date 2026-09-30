#!/usr/bin/env bash
# Regtest lab for lntrace: CLN sender + LDK intermediaries.
#
# Prerequisites:
#   - bitcoind (regtest)
#   - lightningd (Core Lightning) with xpay plugin
#   - lntrace-cln plugin (built from adapters/cln)
#
# This script is a skeleton. Adjust paths for your environment.

set -euo pipefail

DATADIR="${LNTRACE_LAB:-/tmp/lntrace-lab}"
BITCOIN_DIR="$DATADIR/bitcoin"
CLN_DIR="$DATADIR/cln"
NETWORK="regtest"

echo "=== lntrace regtest lab ==="
echo "Data directory: $DATADIR"

# --- 1. Start bitcoind ---
mkdir -p "$BITCOIN_DIR"

if ! pgrep -f "bitcoind.*-regtest.*$BITCOIN_DIR" > /dev/null; then
    echo "Starting bitcoind..."
    bitcoind \
        -regtest \
        -datadir="$BITCOIN_DIR" \
        -server \
        -rpcuser=lntrace \
        -rpcpassword=lntrace \
        -rpcport=18443 \
        -fallbackfee=0.00001 \
        -daemon
    sleep 2
fi

BCLI="bitcoin-cli -regtest -datadir=$BITCOIN_DIR -rpcuser=lntrace -rpcpassword=lntrace -rpcport=18443"

# Mine initial blocks if needed.
BLOCKS=$($BCLI getblockcount 2>/dev/null || echo 0)
if [ "$BLOCKS" -lt 101 ]; then
    echo "Mining initial blocks..."
    ADDR=$($BCLI getnewaddress)
    $BCLI generatetoaddress 101 "$ADDR" > /dev/null
fi

# --- 2. Start CLN (sender) ---
mkdir -p "$CLN_DIR"

# Path to the lntrace CLN recorder plugin (Stage 0).
# After Stage 1, switch to the lntrace-cln adapter plugin.
PLUGIN_PATH="$(cd "$(dirname "$0")/.." && pwd)/target/debug/lntrace-cln-recorder"

if [ ! -f "$PLUGIN_PATH" ]; then
    echo "Building lntrace-cln-recorder..."
    (cd "$(dirname "$0")/.." && cargo build -p lntrace-cln --bin lntrace-cln-recorder)
fi

if ! pgrep -f "lightningd.*$CLN_DIR" > /dev/null; then
    echo "Starting CLN..."
    lightningd \
        --network=$NETWORK \
        --lightning-dir="$CLN_DIR" \
        --bitcoin-rpcuser=lntrace \
        --bitcoin-rpcpassword=lntrace \
        --bitcoin-rpcport=18443 \
        --log-level=debug \
        --plugin="$PLUGIN_PATH" \
        --daemon
    sleep 2
fi

LCLI="lightning-cli --network=$NETWORK --lightning-dir=$CLN_DIR"

echo ""
echo "CLN node ID: $($LCLI getinfo | jq -r '.id')"
echo "CLN is ready."

# --- 3. LDK nodes (intermediaries) ---
# TODO: Start LDK Node instances. Options:
#   a) Build a minimal LDK Node binary in the lab/ directory.
#   b) Use ldk-node's example or ldk-server when available.
#   c) For Stage 0-1 (CLN-only), skip this and use a second CLN node
#      or test with a two-node CLN setup.

echo ""
echo "=== Lab running ==="
echo "  bitcoind RPC: localhost:18443"
echo "  CLN socket:   $CLN_DIR/$NETWORK/lightning-rpc"
echo "  Raw log:      $CLN_DIR/lntrace-raw.jsonl (recorder)"
echo "  Event log:    events.jsonl (adapter)"
echo ""
echo "Next steps:"
echo "  1. Open channels:  $LCLI fundchannel <peer_id> <amount>"
echo "  2. Send payments:  $LCLI xpay <bolt11>"
echo "  3. View events:    cargo run -p lntrace -- events"
echo "  4. Trace payment:  cargo run -p lntrace -- trace <hash>"
