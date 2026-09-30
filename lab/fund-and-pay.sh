#!/usr/bin/env bash
# Fund channels and send test payments for fixture capture.
# Run this after docker compose up has started all containers.
set -euo pipefail

BCLI="docker exec lntrace-bitcoind bitcoin-cli -regtest -rpcuser=lntrace -rpcpassword=lntrace"
LCLI_A="docker exec lntrace-cln-sender lightning-cli --lightning-dir=/home/clightning/.lightning --network=regtest"
LCLI_B="docker exec lntrace-cln-receiver lightning-cli --lightning-dir=/home/clightning/.lightning --network=regtest"

echo "=== Waiting for nodes to sync ==="
# Wait for CLN nodes to be responsive
for node in "$LCLI_A" "$LCLI_B"; do
    for i in $(seq 1 30); do
        if $node getinfo > /dev/null 2>&1; then break; fi
        echo "  waiting for node... (attempt $i/30)"
        sleep 2
    done
done

SENDER_ID=$($LCLI_A getinfo | jq -r '.id')
RECEIVER_ID=$($LCLI_B getinfo | jq -r '.id')
echo ""
echo "Sender ID:   $SENDER_ID"
echo "Receiver ID: $RECEIVER_ID"

# --- 1. Mine initial blocks and fund the sender ---
echo ""
echo "=== Creating bitcoind wallet ==="
$BCLI createwallet "default" 2>/dev/null || $BCLI loadwallet "default" 2>/dev/null || echo "Wallet already loaded"

echo "=== Mining initial blocks ==="
BLOCKS=$($BCLI getblockcount)
if [ "$BLOCKS" -lt 101 ]; then
    ADDR=$($BCLI getnewaddress)
    $BCLI generatetoaddress 101 "$ADDR" > /dev/null
    echo "Mined 101 blocks"
fi

# Wait for CLN nodes to sync to the blockchain
echo "Waiting for CLN nodes to sync..."
for i in $(seq 1 60); do
    HEIGHT_A=$($LCLI_A getinfo | jq -r '.blockheight')
    HEIGHT_B=$($LCLI_B getinfo | jq -r '.blockheight')
    CHAIN_HEIGHT=$($BCLI getblockcount)
    if [ "$HEIGHT_A" -ge "$CHAIN_HEIGHT" ] && [ "$HEIGHT_B" -ge "$CHAIN_HEIGHT" ]; then
        echo "  Both nodes synced to block $CHAIN_HEIGHT"
        break
    fi
    echo "  sender=$HEIGHT_A receiver=$HEIGHT_B chain=$CHAIN_HEIGHT (attempt $i/60)"
    sleep 2
done

# Fund the sender wallet
echo "=== Funding sender wallet ==="
SENDER_ADDR=$($LCLI_A newaddr | jq -r '.bech32')
$BCLI sendtoaddress "$SENDER_ADDR" 1.0 > /dev/null
$BCLI generatetoaddress 6 "$($BCLI getnewaddress)" > /dev/null
echo "Sent 1 BTC to sender, mined 6 blocks"

# Wait for CLN to see the funds
echo "Waiting for CLN to detect funds..."
for i in $(seq 1 30); do
    FUNDS=$($LCLI_A listfunds | jq '.outputs | length')
    if [ "$FUNDS" -gt 0 ]; then
        echo "  Sender has $FUNDS output(s)"
        break
    fi
    echo "  no funds yet (attempt $i/30)"
    sleep 2
done
echo "Sender balance:"
$LCLI_A listfunds | jq '{outputs: [.outputs[] | {amount_msat, status}]}'

# --- 2. Connect and open channel ---
echo ""
echo "=== Connecting sender → receiver ==="
$LCLI_A connect "$RECEIVER_ID@lntrace-cln-receiver:9735" | jq '.'

echo "=== Opening channel (1,000,000 sat) ==="
FUND_RESULT=$($LCLI_A fundchannel "$RECEIVER_ID" 1000000)
echo "$FUND_RESULT" | jq '.'
TXID=$(echo "$FUND_RESULT" | jq -r '.txid')

# Mine to confirm channel
$BCLI generatetoaddress 6 "$($BCLI getnewaddress)" > /dev/null
echo "Mined 6 blocks to confirm channel"

# Wait for channel to be active
echo "Waiting for channel to become active..."
for i in $(seq 1 30); do
    STATE=$($LCLI_A listpeerchannels | jq -r '.channels[0].state // "none"')
    if [ "$STATE" = "CHANNELD_NORMAL" ]; then
        echo "Channel active!"
        break
    fi
    echo "  state: $STATE (attempt $i/30)"
    sleep 2
done

$LCLI_A listpeerchannels | jq '.channels[] | {short_channel_id, state, spendable_msat, receivable_msat}'

# --- 3. Send a successful payment ---
echo ""
echo "=== Test 1: Successful payment (50,000 sat) ==="
INVOICE=$($LCLI_B invoice 50000000 "test-success-$(date +%s)" "lntrace test payment" | jq -r '.bolt11')
echo "Invoice: ${INVOICE:0:40}..."

PAY_RESULT=$($LCLI_A xpay "$INVOICE" 2>&1) || true
echo "Pay result:"
echo "$PAY_RESULT" | jq '.' 2>/dev/null || echo "$PAY_RESULT"

PAYMENT_HASH=$(echo "$PAY_RESULT" | jq -r '.payment_hash // empty' 2>/dev/null)
if [ -n "$PAYMENT_HASH" ]; then
    echo ""
    echo "SUCCESS payment_hash: $PAYMENT_HASH"
fi

# --- 4. Send a payment that will fail ---
echo ""
echo "=== Test 2: Failed payment (invoice to unknown node) ==="
# Create an invoice for more than the channel can handle
BIG_INVOICE=$($LCLI_B invoice 900000000 "test-fail-$(date +%s)" "should fail - too large" | jq -r '.bolt11')
echo "Large invoice: ${BIG_INVOICE:0:40}..."

# Drain most liquidity first by paying close to capacity
DRAIN_INVOICE=$($LCLI_B invoice 800000000 "drain-$(date +%s)" "drain channel" | jq -r '.bolt11')
DRAIN_RESULT=$($LCLI_A xpay "$DRAIN_INVOICE" 2>&1) || true
echo "Drain result:"
echo "$DRAIN_RESULT" | jq '.payment_hash' 2>/dev/null || echo "$DRAIN_RESULT"

# Now try another payment that should fail due to insufficient liquidity
echo ""
echo "=== Test 3: Payment that fails (insufficient liquidity) ==="
FAIL_INVOICE=$($LCLI_B invoice 150000000 "test-fail2-$(date +%s)" "should fail - no liquidity" | jq -r '.bolt11')
FAIL_RESULT=$($LCLI_A xpay "$FAIL_INVOICE" 2>&1) || true
echo "Fail result:"
echo "$FAIL_RESULT" | jq '.' 2>/dev/null || echo "$FAIL_RESULT"

# --- 5. Copy raw recorder output ---
echo ""
echo "=== Captured data ==="
echo "Checking for recorder output..."
docker exec lntrace-cln-sender sh -c "ls -la /home/clightning/.lightning/regtest/lntrace-raw.jsonl 2>/dev/null || echo 'no recorder output yet'"
docker exec lntrace-cln-sender sh -c "wc -l /home/clightning/.lightning/regtest/lntrace-raw.jsonl 2>/dev/null || echo '0 lines'"

echo ""
echo "=== Copying fixtures to host ==="
docker cp lntrace-cln-sender:/home/clightning/.lightning/regtest/lntrace-raw.jsonl ./fixtures/cln-regtest-raw.jsonl 2>/dev/null || echo "No raw file to copy"

echo ""
echo "=== Done ==="
echo "Raw fixture: fixtures/cln-regtest-raw.jsonl"
echo ""
echo "To inspect: cat fixtures/cln-regtest-raw.jsonl | jq ."
echo "To view sender log: docker exec lntrace-cln-sender cat /home/clightning/.lightning/debug.log | tail -50"
