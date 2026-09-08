#[path = "support/process_sample.rs"]
mod process_sample;
#[path = "support/resource_analysis.rs"]
mod resource_analysis;
#[path = "support/resource_matrix.rs"]
mod resource_matrix;
#[path = "support/resource_protocol.rs"]
mod resource_protocol;

#[test]
#[ignore = "executes the frozen resource matrix; requires explicit root, pinned harness, and build manifest"]
fn resource_matrix_campaign() {
    resource_matrix::run().unwrap();
}
