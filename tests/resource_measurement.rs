#[path = "support/process_sample.rs"]
mod process_sample;
#[path = "support/resource_analysis.rs"]
mod resource_analysis;
#[path = "support/resource_collection.rs"]
mod resource_collection;
#[path = "support/resource_counters.rs"]
mod resource_counters;
#[path = "support/resource_matrix.rs"]
mod resource_matrix;
#[path = "support/resource_protocol.rs"]
mod resource_protocol;
#[path = "support/resource_windows.rs"]
mod resource_windows;

#[test]
#[ignore = "audits a completed P2P_VPN_COLLECTION_ROOT into new P2P_VPN_COLLECTION_OUTPUT"]
fn resource_collection_audit() {
    resource_collection::run().unwrap();
}

#[test]
#[ignore = "summarizes P2P_VPN_RESOURCE_OBSERVATIONS into a new P2P_VPN_RESOURCE_SUMMARY file"]
fn resource_summarize_observations() {
    resource_windows::run().unwrap();
}

#[test]
#[ignore = "executes the frozen resource matrix; requires explicit root, pinned harness, and build manifest"]
fn resource_matrix_campaign() {
    resource_matrix::run().unwrap();
}
