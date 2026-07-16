# GitHub Actions CI

- Blocking CI definition lives in `.github/workflows/ci.yml`; exact commands are owned only by `scripts/ci-checks.sh`. `scripts/pre-pr-checks.sh` is a compatibility wrapper that invokes `scripts/ci-checks.sh all`.
- Since Session 17, `format-guardrails` also runs `bash -n` for runtime acceptance scripts and `scripts/tests/playback-smoke-self-test.sh`; these checks validate CLI parsing and full current-schema config generation/production parse without GUI. Runtime hardware/media acceptance remains separate. See `mem:testing/playback-smoke`.
- Stable blocking check names are: `Format and guardrails`, `Strict Clippy`, `Documentation`, `Workspace tests (all features)`, `app-egui (no default features)`, and `MSRV (Rust 1.92.0)`.
- Session 06 adds four independent matrix statuses `Dependency patch (cros-libva)`, `Dependency patch (cros-codecs)`, `Dependency patch (symphonia-format-isomp4)`, `Dependency patch (symphonia-codec-aac)`, plus `Dependency patch integration`. Direct jobs run each standalone manifest/lock; integration invokes `scripts/ci-checks.sh dependency-patches`.
- CI uses Ubuntu 24.04, `actions/checkout@v4`, `actions/cache@v4`, exact cache identities by OS/arch/toolchain/check/manifests, locked Cargo commands, and explicit native build packages: clang, libclang-dev, libasound2-dev, libavcodec-dev, libavutil-dev, libgbm-dev, libva-dev, pkg-config.
- Real GPU/VA-API/audio/display acceptance is not a blocking hosted-runner test. `.github/workflows/hardware-acceptance.yml` is manual and targets `[self-hosted, linux, x64, rustiplayer-hardware]`; it invokes the existing `scripts/playback-smoke.sh --mode full` with explicit real fixture paths. Local and workflow acceptance therefore share the same runner and no software stub substitutes for hardware.
- Human documentation and exact branch-protection checklist live in `docs/continuous-integration.md`.
- Operational decision 2026-07-10: the repository intentionally remains private without GitHub Pro. GitHub API returned HTTP 403 `Upgrade to GitHub Pro or make this repository public` for both repository rulesets and `main` branch protection. Required-check enforcement is explicitly disabled/deferred; workflow failures remain visible but merge control is manual. Session 04 is accepted complete under this owner-approved limitation. When the repository becomes public, enable PR-required/up-to-date required checks using the documented exact names.
- Full `scripts/pre-pr-checks.sh` passed outside sandbox after the CI runner refactor, including guardrails, rustfmt, strict all-features/all-targets Clippy, strict rustdoc, all-features workspace tests, `app-egui --no-default-features`, and Rust 1.92.0 MSRV check.
- Session 07B adds stable CI check `Coverage ratchet`. It installs exact `cargo-llvm-cov 0.8.7`, runs `scripts/coverage.sh check`, validates PR baseline decreases against target-branch baseline plus bounded exceptions, and uploads JSON/LCOV/HTML/raw profile artifacts as `coverage-report`. Detailed policy: `mem:testing/coverage`.

## Dependency policy (Session 05, 2026-07-10)
- CI has a stable blocking `Dependency policy` job that installs exact `cargo-deny 0.20.2` and `cargo-machete 0.9.2`, then runs `scripts/ci-checks.sh dependencies`.
- The runner always executes blocking advisories, non-blocking yanked/unmaintained visibility, licenses/sources/duplicate inventory, and unused direct-dependency analysis before returning aggregate status.
- Current gate is intentionally blocked by RUSTSEC-2026-0194/0195: `wayland-scanner 0.31.10 -> quick-xml 0.39.3`; no ignore or policy weakening was added. See `docs/dependency-report-2026-07-10.md`.

## Session 28 readiness audit (2026-07-12)

- Local locked format/check/strict Clippy/docs/all-features tests/app-no-default/MSRV and all four direct patch suites pass on `a9d3c86`.
- `scripts/ci-checks.sh dependencies` remains blocking on RUSTSEC-2026-0194/0195 (`wayland-scanner 0.31.10 -> quick-xml 0.39.3`); licenses/sources/bans pass and cargo-machete reports no unused direct deps. RUSTSEC-2026-0150 marks `audiopus_sys 0.2.2` unmaintained as non-blocking visibility.
- Coverage ratchet is also currently red due the external-test filename classification described in `mem:testing/coverage`; do not treat a green unit suite as overall CI readiness.
- GitHub rulesets and `main` protection APIs were rechecked and still return the private-repo-without-Pro HTTP 403 limitation. Full evidence: root `readiness_report_2026-07-12.md`.

## Playlist Session 21 dependency inventory (2026-07-16)

- `scripts/ci-checks.sh` владеет exact `WORKSPACE_CRATE_DIRECTORIES` из всех 37 root workspace members для `cargo machete --with-metadata`; четыре standalone patch crates из workspace `exclude` намеренно не попадают в этот recursive audit.
- `scripts/tests/test_dependency_audit_inventory.py` сверяет exact set, uniqueness и disjoint exclusions с root `Cargo.toml`. Это предотвращает повторный пропуск новых crates; policy discovery сейчас 30 tests.
- Session 21 dependency run проверил все 37 crates: licenses/bans/sources и cargo-machete прошли, а общий gate честно FAIL только на прежних `RUSTSEC-2026-0194/0195` (`wayland-scanner 0.31.10 -> quick-xml 0.39.3`). Advisory ignores не добавлялись; foundation остаётся NOT READY.
