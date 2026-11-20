//! Examples gate: the example hosts must keep compiling.
//!
//! `http-cache/runtime.rs` and `orm/runtime.rs` are server hosts that never
//! exit, so they are **build-only** here: `cargo build --locked --examples`
//! from the package root, and exit status 0 is the whole assertion. Behaviour
//! is not tested here: the suites in `crates/*/tests` drive the libraries,
//! and the component rungs boot these runtimes in-process. Anything a host links against (the omnia runtime, the `Hooks`
//! wiring, the manifest) drifting far enough to break an example fails this
//! test before it fails a manual `make run <name>`.
//!
//! Nextest runs the test in its own process; concurrent invocations
//! serialise on cargo's build lock, so the nested build is safe to repeat.

use std::process::Command;

/// Run `cargo --locked <args>` from the package root and require success.
fn cargo(args: &[&str]) {
    let status = Command::new(env!("CARGO"))
        .arg("--locked")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("spawn cargo");
    assert!(status.success(), "cargo --locked {} failed", args.join(" "));
}

/// Every `[[example]]` in the package manifest compiles.
#[test]
fn build() {
    cargo(&["build", "--examples"]);
}
