//! The guest scenario programs the extension e2e suites drive, from both
//! sides of the boundary.
//!
//! Each program under `programs/<crate>/<scenario>.rs` is an `[[example]]`
//! compiled to a component. Natively the crate is the compiled artifacts:
//! `build.rs` runs that `wasm32-wasip3` build and generates one
//! `pub const <NAME>: &str` path per program plus a `foreach_<crate>!` macro
//! a suite invokes to prove every program has a matching test. A suite runs
//! an artifact through `omnia_test::host`.

#[cfg(not(target_arch = "wasm32"))]
include!(concat!(env!("OUT_DIR"), "/gen.rs"));
