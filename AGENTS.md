# omnia-ext — Agent Instructions

Guest-side libraries that compose on [omnia](https://github.com/augentic/omnia) capability traits; omnia never depends on this repository. [README.md](README.md) is the guide: the [crates](README.md#crates), the [testing rungs](README.md#testing), and the [development loop](README.md#development). The rules below are the organisation's shared conventions; where the README is more specific, it governs.

## Git

Never `git commit`, `git push`, open or close a pull request, or delete a branch — in this repository or in any sibling checkout — unless the maintainer lifts this for the session, explicitly and for named work. Leave every change uncommitted in the working tree; the maintainer reviews and commits. No plan or to-do list carries a commit, push, or PR step, and an instruction to complete every step does not override this.

## Code style

clippy (`make lint`) and nightly rustfmt (`make fmt`) are the style gate; beyond them and the rules below, match the surrounding code.

- Suppress a lint with `#[expect(lint, reason = "…")]` at the smallest scope, never `#[allow]`.
- `<module>.rs` plus `<module>/<child>.rs`; `mod.rs` only under `tests/support/`.
- Order a module top-down. Primary `pub` entry-point fns and top-level types come first. A type's inherent `impl`, trait `impl`s, and child types follow it immediately before the next top-level item. A private helper sits immediately below its first caller, and `#[cfg(test)] mod tests` sits last.
- A fn over a type is that type's method, not a free fn taking it as its first argument, where the type's module declares the fn or the fn is a plain lookup or predicate on the type. A constructor is an associated fn. A policy `const` sits beside the type whose method reads it. Values several fns thread through every call become one struct whose methods they are. A fn stays free when it is pure over primitives and iterators, or when it is one module's rule applied to another module's type.

Comments follow the conventions `std`, `serde`, and `tokio` converge on: docs state the observable contract for the crate's user, never the body's mechanics.

- `///` goes on the public API only — the `pub` types, fns, fields, variants, and re-exports a user of the crate can reach — never on a private or `pub(crate)` item, an `impl` block, or a trait-impl method. A clap field's `///` is its `--help` text. A doc opens with one summary sentence (about fifteen words, full stop), then a blank line, then short sentences and bullet lists. `# Examples` holds compiled doctests, for non-obvious usage only; `# Errors` names each class the caller matches on, linked; `# Panics` the rest. Every item mentioned is an intra-doc link. No mechanics, history, or migration notes. A `//!` says what a module is for, in the same shape.
- A private item takes a `//` only for what a senior developer would not see from its name and signature: a constraint, a why, an invariant. Most carry nothing. No restatements, match-arm labels, or body paraphrases.
- Inside a body, a `//` is a section header: lowercase, no full stop, above a blank-line-separated block, naming what the block achieves, so the headers read together outline the fn. A fn readable at a glance carries none, and a header never narrates the line beneath it. The one in-body explanation is `// HACK: …`, for a trick a senior would not see through.
- A test fn takes `//`, never `///`, and only for rationale its scenario name and assertions do not expose.
- No commented-out code.
- Every sentence earns its place and reads once: short plain sentences, one idea each; three or more things are a bullet list, not a colon-and-dash clause; no chained em-dashes, nested parentheticals, or semicolon runs; each fact has one home across `//!`, `///`, and `//`. A comment is as long as its why takes and no longer — concise is not dense, and readable is not verbose.

## Testing

Tests drive the public boundary: a behaviour is asserted through what a user of the product or crate can reach, over scripted doubles rather than a live filesystem, network, or model, never through private internals. A suite below the root survives only for an independent library contract; a unit test only for a branch no public boundary reaches. A test fn names the scenario (`gen_spec`, `no_sources`), never the outcome. Scripted doubles are strict: script exactly the exchanges a run consumes.

## Commands

All from the repository root through `make` ([`Makefile`](Makefile) → mise). The tasks are the shared `mise/rust.toml` of [`augentic/toolkit`](https://github.com/augentic/toolkit), pinned in [`mise.toml`](mise.toml) to the tag every `uses:` under `.github/workflows/` names; bump both together in one pull request.

```bash
make ci # exactly the CI jobs: fmt-check + lint + test + test-docs + docs + vet + deny — run before handing over
make check # local advisories: audit + fmt (rewrites) + lint + outdated + deps
make test # cargo nextest run --locked --workspace --all-features, under -Dwarnings
make lint # lint-host (cargo clippy --workspace --all-targets --all-features, then cargo hack --each-feature), then lint-wasm (the same over every lib, bin and example for wasm32-wasip2 — never tests)
make fmt # cargo +nightly fmt --all
make vet-regen # regenerate cargo-vet imports/exemptions/unpublished, then vet
make cov # cargo llvm-cov nextest --workspace --all-features --summary-only
make sweep # drop target/ artifacts untouched for a week
```

If `make ci` cannot run, say exactly why and which checks ran instead.
