use serde::{Deserialize, Serialize};

// BOLT 4 failure code flag bits.
pub const BADONION: u16 = 0x8000;
pub const PERM: u16 = 0x4000;
pub const NODE: u16 = 0x2000;
pub const UPDATE: u16 = 0x1000;

/// Decoded BOLT 4 failure code with flags and human-readable explanation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureExplanation {
    /// Raw 16-bit failure code.
    pub code: u16,
    /// Base code (lower 12 bits).
    pub base: u16,
    /// Failure name from the spec.
    pub name: &'static str,
    /// Plain-language explanation.
    pub explanation: &'static str,
    /// Actionable guidance.
    pub guidance: &'static str,
    pub is_permanent: bool,
    pub is_node: bool,
    pub is_update: bool,
    pub is_badonion: bool,
}

/// Decode a BOLT 4 failure code into a human-readable explanation.
pub fn explain(code: u16) -> FailureExplanation {
    let base = code & 0x0FFF;
    let is_permanent = code & PERM != 0;
    let is_node = code & NODE != 0;
    let is_update = code & UPDATE != 0;
    let is_badonion = code & BADONION != 0;

    let (name, explanation, guidance) = match code {
        0x4001 => (
            "invalid_realm",
            "The realm byte in the onion was not understood by the processing node.",
            "This usually indicates a software version mismatch. Update the node software.",
        ),
        0x2002 => (
            "temporary_node_failure",
            "The node is temporarily unavailable.",
            "Retry after a delay. The node may be restarting or overloaded.",
        ),
        0x6002 => (
            "permanent_node_failure",
            "The node has failed permanently and will not process payments.",
            "Exclude this node from future routes.",
        ),
        0x6003 => (
            "required_node_feature_missing",
            "The node requires a feature that the sender did not set.",
            "Check that both nodes support the required features.",
        ),
        0xC004 => (
            "invalid_onion_version",
            "The onion version is not recognized.",
            "This may indicate a protocol version mismatch.",
        ),
        0xC005 => (
            "invalid_onion_hmac",
            "The onion HMAC is incorrect — the onion was corrupted or tampered with.",
            "Retry on a different path. If persistent, a node may be misbehaving.",
        ),
        0xC006 => (
            "invalid_onion_key",
            "The ephemeral key in the onion is invalid.",
            "Retry on a different path.",
        ),
        0x1007 => (
            "temporary_channel_failure",
            "The channel cannot currently fulfill the HTLC. This code is overloaded: \
             it covers insufficient liquidity, too many in-flight HTLCs, HTLC minimum \
             not met, or a channel that is being closed.",
            "When the failing node is instrumented, lntrace shows the actual cause. \
             Otherwise, retry — liquidity may have shifted.",
        ),
        0x4008 => (
            "permanent_channel_failure",
            "The channel has failed permanently.",
            "Exclude this channel from future routes.",
        ),
        0x4009 => (
            "required_channel_feature_missing",
            "The channel requires a feature not supported by the HTLC.",
            "Check channel feature requirements.",
        ),
        0x400A => (
            "unknown_next_peer",
            "The node does not have a channel with the next peer in the route.",
            "The channel may have been closed. Update the network graph.",
        ),
        0x100B => (
            "amount_below_minimum",
            "The HTLC amount is below the channel's minimum.",
            "Increase the payment amount or route through a different channel.",
        ),
        0x100C => (
            "fee_insufficient",
            "The fee included in the HTLC is too low for the forwarding node.",
            "The fee policy may have changed. Update the network graph and retry.",
        ),
        0x100D => (
            "incorrect_cltv_expiry",
            "The CLTV expiry in the HTLC does not match the expected value.",
            "The channel's CLTV delta may have changed. Update the graph and retry.",
        ),
        0x100E => (
            "expiry_too_soon",
            "The CLTV expiry is too close to the current block height.",
            "The payment took too long to propagate. Retry with a fresh route.",
        ),
        0x400F => (
            "incorrect_or_unknown_payment_details",
            "The final node does not recognize the payment hash, or the amount/expiry \
             is wrong. These cases are merged to prevent probing.",
            "Verify the invoice is correct and has not expired.",
        ),
        0x0011 => (
            "final_expiry_too_soon",
            "The CLTV expiry at the final hop is too close to the current block height.",
            "Retry with a higher min_final_cltv_expiry.",
        ),
        0x0012 => (
            "final_incorrect_cltv_expiry",
            "The CLTV expiry at the final hop does not match the invoice.",
            "Check the invoice's min_final_cltv_expiry.",
        ),
        0x0013 => (
            "final_incorrect_htlc_amount",
            "The amount at the final hop does not match the invoice.",
            "Check that the payment amount matches the invoice exactly.",
        ),
        0x1014 => (
            "channel_disabled",
            "The channel has been disabled by the forwarding node.",
            "The channel may be temporarily offline. Try a different route.",
        ),
        0x0015 => (
            "expiry_too_far",
            "The CLTV expiry is unreasonably far in the future.",
            "Reduce the route length or CLTV delta.",
        ),
        0x4016 => (
            "invalid_onion_payload",
            "A TLV payload in the onion is malformed or contains unknown required fields.",
            "This may indicate a protocol or feature mismatch.",
        ),
        0x0017 => (
            "mpp_timeout",
            "The recipient did not receive all parts of a multi-part payment within the \
             timeout window (~60 seconds). One or more shards failed to arrive, so the \
             receiver failed all partial HTLCs back.",
            "Retry the entire payment. Check which shard failed and why \
             (it will have its own failure code).",
        ),
        _ => (
            "unknown",
            "Unrecognized failure code.",
            "Check the BOLT 4 specification for this code.",
        ),
    };

    FailureExplanation {
        code,
        base,
        name,
        explanation,
        guidance,
        is_permanent,
        is_node,
        is_update,
        is_badonion,
    }
}

/// Format a failure explanation for terminal output.
pub fn format_failure(code: u16) -> String {
    let f = explain(code);
    let mut flags = Vec::new();
    if f.is_permanent {
        flags.push("PERM");
    }
    if f.is_node {
        flags.push("NODE");
    }
    if f.is_update {
        flags.push("UPDATE");
    }
    if f.is_badonion {
        flags.push("BADONION");
    }
    let flag_str = if flags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", flags.join("|"))
    };

    format!(
        "{} (0x{:04X}){}\n  {}\n  -> {}",
        f.name, f.code, flag_str, f.explanation, f.guidance
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_channel_failure_is_update() {
        let f = explain(0x1007);
        assert_eq!(f.name, "temporary_channel_failure");
        assert!(f.is_update);
        assert!(!f.is_permanent);
    }

    #[test]
    fn mpp_timeout_has_no_flags() {
        let f = explain(0x0017);
        assert_eq!(f.name, "mpp_timeout");
        assert!(!f.is_permanent);
        assert!(!f.is_node);
        assert!(!f.is_update);
    }

    #[test]
    fn unknown_code_returns_unknown() {
        let f = explain(0xFFFF);
        assert_eq!(f.name, "unknown");
    }
}
