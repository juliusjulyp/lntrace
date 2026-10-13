//! Shared channel polling utilities.

use cln_rpc::model::requests::ListpeerchannelsRequest;

/// Execute a `listpeerchannels` RPC call.
pub async fn list_peer_channels(
    rpc: &mut cln_rpc::ClnRpc,
) -> Result<cln_rpc::model::responses::ListpeerchannelsResponse, anyhow::Error> {
    let req = ListpeerchannelsRequest {
        id: None,
        channel_id: None,
        short_channel_id: None,
    };
    Ok(rpc.call_typed(&req).await?)
}
