#[path = "support/process_sample.rs"]
mod process_sample;
#[path = "support/resource_analysis.rs"]
mod resource_analysis;
#[path = "support/resource_matrix.rs"]
mod resource_matrix;
#[path = "support/resource_protocol.rs"]
mod resource_protocol;
#[path = "support/resource_windows.rs"]
mod resource_windows;

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
