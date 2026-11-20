//! Compiles the example guest to a `wasm32-wasip3` component for the
//! component rung (`tests/orm.rs`) and generates `gen.rs` with its path
//! constant (`ORM_WASM`) for the native side to `include!`.
//!
//! The nested build compiles this same package for `wasm32`, running this
//! script again; `Components` is a no-op under that target, so the recursion
//! stops there. The fixture uses the dev profile and lands in
//! `target/wasm32-fixtures`, a sibling of the outer profile directory shared
//! by every outer feature set, profile, and build-script hash, so plain
//! `cargo test` is self-contained and a version or toolchain bump rebuilds
//! the tree incrementally instead of leaving the last one behind under a
//! per-hash `OUT_DIR`.

fn main() {
    omnia_test::build::Components::in_workspace("..")
        .package("examples")
        .examples(["orm-wasm"])
        .group("example")
        // `Cargo.lock` is outside the dep-info the nested build emits.
        .track(["Cargo.lock"])
        .build()
        .write_gen("gen.rs");
}
