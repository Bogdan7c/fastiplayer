# S42 core/services dedicated unit-test layout (2026-08-27)

- Behavior-neutral test relocation only; production API, module ownership, runtime behavior, test names and assertions are unchanged.
- `crates/capability-core/src/selection.rs` declares `#[cfg(test)] mod tests;`; its unit tests live in `crates/capability-core/src/selection/tests.rs`.
- `crates/hds-manifest-core/src/bootstrap.rs` declares `#[cfg(test)] mod tests;`; its unit tests live in `crates/hds-manifest-core/src/bootstrap/tests.rs`.
- `crates/service-direct-media/src/lib.rs` declares `#[cfg(test)] mod tests;`; its unit tests live in `crates/service-direct-media/src/tests.rs`.
- `crates/source-core/src/http.rs` declares `#[cfg(test)] mod tests;`; its main HTTP range unit tests live in `crates/source-core/src/http/tests.rs`; the existing sibling `range_redirect_tests` and `error_mapping_tests` modules remain unchanged.
- The external modules remain private descendants of their production parents, so existing `use super::*` access to private parent items is preserved under Rust privacy rules.
- Rustfmt-normalized bodies were compared to HEAD and matched exactly. Focused relocated suites passed 48 tests with one pre-existing manual ignore. The Rust 1.96.0 combined all-features locked suite for capability-core, hds-manifest-core, service-direct-media and source-core passed 135 tests with the same one manual ignore.
- `scripts/module-size-baseline.json` was intentionally not changed in this relocation wave. `scripts/check_s42_guardrails.py` therefore reports the four expected stale/shrunk production-module baseline rows until the owner performs the coordinated baseline reconciliation.