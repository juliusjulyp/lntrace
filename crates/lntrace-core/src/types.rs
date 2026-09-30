use serde::{Deserialize, Serialize};
use std::fmt;

/// Hex-encoded compressed public key (33 bytes, 66 hex chars).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show first 8 and last 8 hex chars for readability.
        if self.0.len() > 16 {
            write!(f, "{}...{}", &self.0[..8], &self.0[self.0.len() - 8..])
        } else {
            write!(f, "{}", self.0)
        }
    }
}

/// BOLT short_channel_id: block << 40 | tx_index << 16 | output_index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShortChannelId(pub u64);

impl ShortChannelId {
    pub fn block(&self) -> u32 {
        (self.0 >> 40) as u32
    }
    pub fn tx_index(&self) -> u32 {
        ((self.0 >> 16) & 0xFFFFFF) as u32
    }
    pub fn output_index(&self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }
    /// Parse "block x tx x output" format (e.g. "103x1x0").
    pub fn from_str_bolt(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('x').collect();
        if parts.len() != 3 {
            return None;
        }
        let block: u64 = parts[0].parse().ok()?;
        let tx: u64 = parts[1].parse().ok()?;
        let out: u64 = parts[2].parse().ok()?;
        Some(ShortChannelId((block << 40) | (tx << 16) | out))
    }
}

impl fmt::Display for ShortChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{}x{}",
            self.block(),
            self.tx_index(),
            self.output_index()
        )
    }
}

/// Funding transaction outpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FundingOutpoint {
    pub txid: String,
    pub vout: u32,
}

impl fmt::Display for FundingOutpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.txid, self.vout)
    }
}

/// Channel identifier. At least one of scid or funding should be present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelId {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scid: Option<ShortChannelId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding: Option<FundingOutpoint>,
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.scid, &self.funding) {
            (Some(scid), _) => write!(f, "{scid}"),
            (None, Some(fp)) => write!(f, "{fp}"),
            (None, None) => write!(f, "<unknown>"),
        }
    }
}

/// A single hop in a payment route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hop {
    pub node_id: NodeId,
    pub channel: ChannelId,
    pub amount_msat: u64,
    pub fee_msat: u64,
    pub cltv_expiry: u32,
}

/// Close reason for a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CloseReason {
    Cooperative,
    Force,
    Breach,
    Unknown(String),
}

/// Forward status as observed by an intermediate node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ForwardStatus {
    Offered,
    Settled,
    Failed,
    LocalFailed,
}

/// Confidence marker for correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Confidence {
    /// Matched by payment hash — certain.
    Exact,
    /// Matched by channel adjacency, amount, and timing — marked as inferred.
    Inferred,
}
