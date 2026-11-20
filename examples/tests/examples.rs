//! Examples gate: the example host must keep compiling.
//!
//! `orm/runtime.rs` is a server host that never exits, so it is
//! **build-only** here: `cargo build --locked --examples` from the package
//! root, and exit status 0 is the whole assertion. Behaviour is not tested
//! here: the suite in `crates/orm/tests` drives the library, and the
//! component rung boots this runtime in-process. Anything a host links
//! against (the omnia runtime, the `Hooks` wiring, the manifest) drifting far
//! enough to break an example fails this test before it fails a manual
//! `make run <name>`.
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
