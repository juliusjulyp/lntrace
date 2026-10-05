//! Infer the actual cause of a `temporary_channel_failure` (0x1007) using
//! channel state snapshots from the failing node.
//!
//! Pure functions: snapshot in, `Option<InferredCause>` out, no I/O.

use crate::event::ChannelSnapshot;
use serde::{Deserialize, Serialize};

/// An inferred cause with a human-readable description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredCause {
    pub rule: CauseRule,
    pub description: String,
}

/// Which rule matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CauseRule {
    /// Channel state is not CHANNELD_NORMAL.
    ChannelNotUsable,
    /// `spendable_msat < htlc_amount`.
    InsufficientLiquidity,
    /// `htlc_amount < minimum_htlc_out_msat`.
    BelowHtlcMinimum,
    /// `inflight_htlc_count >= max_accepted_htlcs`.
    TooManyHtlcs,
    /// Snapshot looked healthy — cause unknown.
    Unknown,
}

/// Temporary channel failure code (BOLT 4).
const TEMPORARY_CHANNEL_FAILURE: u16 = 0x1007;

/// Infer the actual cause of a failure from a channel snapshot.
///
/// Only fires for failcode `0x1007` (`temporary_channel_failure`).
/// Returns `None` for other codes.
///
/// `htlc_amount_msat` is the amount the HTLC tried to push through
/// this channel (the `channel_out_msat` from the route hop).
pub fn infer_cause(
    failcode: u16,
    htlc_amount_msat: u64,
    snapshot: &ChannelSnapshot,
) -> Option<InferredCause> {
    if failcode != TEMPORARY_CHANNEL_FAILURE {
        return None;
    }

    // Rule 1: channel not in a usable state.
    if let Some(ref state) = snapshot.state {
        if state != "CHANNELD_NORMAL" {
            return Some(InferredCause {
                rule: CauseRule::ChannelNotUsable,
                description: format!("channel not usable (state: {state})"),
            });
        }
    }

    // Rule 2: insufficient outbound liquidity.
    if let Some(spendable) = snapshot.spendable_msat {
        if spendable < htlc_amount_msat {
            return Some(InferredCause {
                rule: CauseRule::InsufficientLiquidity,
                description: format!(
                    "insufficient outbound liquidity (spendable {} msat, HTLC requested {} msat)",
                    spendable, htlc_amount_msat
                ),
            });
        }
    }

    // Rule 3: HTLC amount below channel minimum.
    if let Some(min_htlc) = snapshot.minimum_htlc_out_msat {
        if htlc_amount_msat < min_htlc {
            return Some(InferredCause {
                rule: CauseRule::BelowHtlcMinimum,
                description: format!(
                    "below HTLC minimum (amount {} msat, minimum {} msat)",
                    htlc_amount_msat, min_htlc
                ),
            });
        }
    }

    // Rule 4: too many in-flight HTLCs.
    if let (Some(inflight), Some(max)) = (snapshot.inflight_htlc_count, snapshot.max_accepted_htlcs)
    {
        if inflight >= max {
            return Some(InferredCause {
                rule: CauseRule::TooManyHtlcs,
                description: format!("too many in-flight HTLCs ({inflight} in flight, max {max})"),
            });
        }
    }

    // Rule 5: nothing obvious — state looked healthy.
    Some(InferredCause {
        rule: CauseRule::Unknown,
        description: "unknown (channel state looked healthy)".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelId, NodeId};

    fn base_snapshot() -> ChannelSnapshot {
        ChannelSnapshot {
            channel: ChannelId {
                scid: None,
                funding: None,
            },
            peer: NodeId("peer".into()),
            capacity_sat: 1_000_000,
            local_msat: 500_000_000,
            remote_msat: 500_000_000,
            active: true,
            direction: Some(0),
            state: Some("CHANNELD_NORMAL".into()),
            spendable_msat: Some(400_000_000),
            receivable_msat: Some(400_000_000),
            minimum_htlc_out_msat: Some(1000),
            max_accepted_htlcs: Some(483),
            inflight_htlc_count: Some(0),
        }
    }

    #[test]
    fn non_1007_returns_none() {
        let snap = base_snapshot();
        assert!(infer_cause(0x100A, 50_000_000, &snap).is_none());
    }

    #[test]
    fn channel_not_usable() {
        let mut snap = base_snapshot();
        snap.state = Some("CHANNELD_AWAITING_LOCKIN".into());

        let cause = infer_cause(0x1007, 50_000_000, &snap).unwrap();
        assert_eq!(cause.rule, CauseRule::ChannelNotUsable);
        assert!(cause.description.contains("CHANNELD_AWAITING_LOCKIN"));
    }

    #[test]
    fn insufficient_liquidity() {
        let mut snap = base_snapshot();
        snap.spendable_msat = Some(100_000);

        let cause = infer_cause(0x1007, 50_000_000, &snap).unwrap();
        assert_eq!(cause.rule, CauseRule::InsufficientLiquidity);
        assert!(cause.description.contains("100000 msat"));
        assert!(cause.description.contains("50000000 msat"));
    }

    #[test]
    fn below_htlc_minimum() {
        let mut snap = base_snapshot();
        snap.minimum_htlc_out_msat = Some(100_000);

        let cause = infer_cause(0x1007, 50_000, &snap).unwrap();
        assert_eq!(cause.rule, CauseRule::BelowHtlcMinimum);
        assert!(cause.description.contains("50000 msat"));
        assert!(cause.description.contains("100000 msat"));
    }

    #[test]
    fn too_many_htlcs() {
        let mut snap = base_snapshot();
        snap.inflight_htlc_count = Some(483);
        snap.max_accepted_htlcs = Some(483);

        let cause = infer_cause(0x1007, 50_000_000, &snap).unwrap();
        assert_eq!(cause.rule, CauseRule::TooManyHtlcs);
        assert!(cause.description.contains("483"));
    }

    #[test]
    fn unknown_when_healthy() {
        let snap = base_snapshot();
        let cause = infer_cause(0x1007, 50_000_000, &snap).unwrap();
        assert_eq!(cause.rule, CauseRule::Unknown);
        assert!(cause.description.contains("healthy"));
    }

    #[test]
    fn priority_order_channel_state_first() {
        // Even with insufficient liquidity, channel_not_usable should fire first.
        let mut snap = base_snapshot();
        snap.state = Some("CHANNELD_SHUTTING_DOWN".into());
        snap.spendable_msat = Some(0);

        let cause = infer_cause(0x1007, 50_000_000, &snap).unwrap();
        assert_eq!(cause.rule, CauseRule::ChannelNotUsable);
    }
}
