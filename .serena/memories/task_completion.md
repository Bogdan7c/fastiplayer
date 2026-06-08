# Task Completion

- For any code task, first ensure local workflow was followed: Context7 checked for relevant library/API docs, `code_index` path set to `<REPO_ROOT>`, deep index built, and architecture/test plan stated when the change is non-trivial.
- Run the narrowest relevant tests first: crate-specific `cargo test -p <crate>` for touched workspace crates; include neighboring crates when changing contracts or cross-module boundaries.
- Local `[replace]` patch crates such as `crates/cros-codecs-patch` and `crates/cros-libva-patch` are not workspace members, so `cargo test -p cros-codecs-patch`/direct `--manifest-path` currently fail under this workspace layout. Validate patch-crate changes through dependent workspace crates (for example `video-vaapi`) unless the workspace membership policy is intentionally changed.
- Run `cargo check --workspace` after Rust changes that affect public types, features, workspace deps, or multiple crates.
- Run `cargo clippy --workspace --all-targets` before considering broad quality/Sonar-related work done; for small isolated edits, explain if skipped.
- Run `cargo fmt --all --check`; apply `cargo fmt --all` if formatting fails.
- For broad pre-PR validation, run `scripts/pre-pr-checks.sh`; it includes Cargo metadata sanity, refactor guardrails, fmt, workspace check, and workspace Clippy.
- For playback/seek/render changes, verify relevant behavior path: unit tests plus manual command/log scenario where automated coverage is insufficient.
- For Sonar tasks only on explicit request: run `SONAR_USER_HOME=/tmp/rustiplayer-sonar-user-home scripts/sonar-local-analysis.sh` with `SONAR_TOKEN` only in env, wait for CE background task, group issues by rule/severity/file, fix root cause, rerun relevant tests/clippy/Sonar.
- Before final response, self-review the diff for accidental boundary violations, behavior changes, broad refactors, missing error handling, and tests that only assert symptoms.
- Mention any commands that could not be run and why.