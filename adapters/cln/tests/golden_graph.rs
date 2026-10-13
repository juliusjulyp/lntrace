//! Golden tests for graph builder and trace list using 4-node reroute fixtures.

mod fixtures;
use fixtures::*;

use lntrace::*;

#[test]
fn graph_response_from_reroute() {
    let (envelopes, aliases) = load_reroute();
    let graph = build_graph(&envelopes, &aliases);

    assert_eq!(graph.nodes.len(), 4, "expected 4 nodes");
    assert_eq!(graph.channels.len(), 4, "expected 4 channels");

    // Every node should have an alias.
    for node in &graph.nodes {
        assert!(node.alias.is_some(), "node {} missing alias", node.id);
    }

    insta::assert_yaml_snapshot!("reroute_graph", graph);
}

#[test]
fn trace_list_from_reroute() {
    let (envelopes, _aliases) = load_reroute();
    let traces = correlate(&envelopes);
    let list = build_trace_list(&traces);

    assert!(
        list.len() >= 2,
        "expected at least 2 traces (drain + reroute)"
    );

    // There should be a trace with 2+ attempts (reroute) that succeeded.
    let reroute = list.iter().find(|e| e.attempt_count >= 2);
    assert!(reroute.is_some(), "expected a trace with 2+ attempts");
    assert!(reroute.unwrap().success);

    insta::assert_yaml_snapshot!("reroute_trace_list", list);
}

#[test]
fn single_trace_from_reroute() {
    let (envelopes, _aliases) = load_reroute();
    let traces = correlate(&envelopes);

    // Find the reroute trace (2+ attempts).
    let reroute_trace = traces
        .iter()
        .find(|t| t.attempts.len() >= 2)
        .expect("expected reroute trace");

    insta::assert_yaml_snapshot!("reroute_single_trace", reroute_trace);
}
