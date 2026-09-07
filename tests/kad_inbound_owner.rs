// Compile the production owner and its tests without the vendored crate's
// unrelated upstream test dependencies.
// The shared source is formatted separately using its vendored Rust edition.
#[rustfmt::skip]
#[path = "../vendor/libp2p-kad-0.48.0/src/handler/inbound.rs"]
mod inbound;
