# Current S08/S09 policy override (2026-09-05)

Full measured coverage is workflow_dispatch-only by explicit owner decision in S08; automatic CI retains Coverage baseline policy and functional/quality checks. Historical blocking-coverage paragraphs below are superseded; see `mem:testing/coverage`. Public launch plans allow direct maintainer pushes and prohibit main deletion/force-push; do not infer PR-only rules from historical advice below. Measured source `9165200c` passed CI 33934211412 and Toolchain policy 33934211484. Evidence source `c16da1e7` workflows 33937177688/33937177658 failed before execution because GitHub reported account payments/spending-limit restriction; this is not a test failure.

# GitHub Actions CI

## Public launch S03: all-target CI prerequisites и bounded artifacts (2026-09-04)

- `Workspace tests (all features)` и `Coverage ratchet` компилируют Cargo examples, включая `audio-timestretch/examples/backend_shootout`, который линкуется с системной `libSoundTouch` через crate `soundtouch`/`soundtouch-ffi`.
- GitHub-hosted jobs получают отдельные чистые VM, поэтому оба job явно устанавливают Ubuntu 24.04 packages `libsoundtouch-dev`, `libvulkan1` и `mesa-vulkan-drivers` в собственном `Install native build dependencies` step. SoundTouch закрывает `backend_shootout` link, а Mesa lavapipe предоставляет headless Vulkan adapter для media-to-render functional tests. Отсутствие SoundTouch было подтверждено run `33902335187`; отсутствие Vulkan runtime — 16 одинаковыми `Adapter NotFound` failures в workspace job run `33913206255`. Это CI-environment fallout, а не coverage regression.
- После установки SoundTouch run `33905310274` дважды воспроизводимо исчерпал диск стандартных runners в этих двух jobs (`rustc-LLVM`/runner `System.IO.IOException: No space left on device`). Принятая owner policy: только jobs `tests` и `coverage` задают job-local `CARGO_PROFILE_TEST_DEBUG=0`. Это уменьшает test-profile DWARF artifacts, не меняя тестовый состав, LLVM coverage mapping, baseline или локальные Cargo profiles.
- `scripts/tests/test_ci_native_prerequisites.py::CiNativePrerequisitesTests::test_all_target_jobs_have_bounded_artifacts_and_native_dependencies` закрепляет exact debug profile и native package inventory независимо для jobs `tests` и `coverage`, чтобы настройка одной VM не могла маскировать другую.

## Stable coverage v2 override (2026-08-30)

- Этот раздел supersedes более старые coverage-v1 paragraphs ниже. Authoritative architecture/methodology: `mem:testing/coverage`.
- Stable blocking job/status остаётся `Coverage ratchet`; artifact name остаётся `coverage-report`. Workflow устанавливает exact cargo-llvm-cov 0.8.7 и запускает `scripts/coverage.sh check`.
- На pull request отдельный fail-closed step извлекает из `origin/${{ github.base_ref }}` обе previous tracked части: `coverage/baseline.json` и `coverage/measurement-exceptions.json`. Затем единственный owner `scripts/coverage_stability.py check-baseline-update` получает exact four required previous/proposed flags. Missing base file, malformed pair или policy violation падают; migration fallback/legacy updater/continue-on-error отсутствуют.
- Workflow contract закреплён `scripts/tests/test_s42_release_runner.py::S42ReleaseRunnerTests::test_coverage_check_composes_stable_preflight_suite_and_ratchet` и pure canonical scanner `scripts/tests/coverage_workflow_contract.py`: active unfiltered PR trigger, exact jobs→coverage→steps ancestry, отсутствие job suppression/concurrency override, exact update shell tuple, measured `scripts/coverage.sh check`, upload `if: always()` + `actions/upload-artifact@v4` + stable artifact name.
- Root coverage env остаётся exact `CARGO_INCREMENTAL=0`, `CARGO_TERM_COLOR=always`; coverage job/measured step не могут задавать `RUST_TEST_THREADS`, `if`, `continue-on-error`, `needs` или `strategy`, которые убрали бы обычную three-run concurrency/blocking status.
- Human documentation: `docs/code-coverage.md` и coverage section `docs/continuous-integration.md`.


## Current dependency-gate status after AUD-002 (2026-08-23)

- `scripts/ci-checks.sh dependencies` снова blocking-green после exact lock updates `event-listener 5.4.1 -> 5.4.2` и `webbrowser 1.2.1 -> 1.2.2`; RUSTSEC-2026-0221/0257 отсутствуют.
- `audiopus_sys 0.2.2` / RUSTSEC-2026-0150 и `ttf-parser 0.25.1` / RUSTSEC-2026-0192 остаются только в намеренно nonblocking visibility report, safe upgrade отсутствует.
- Licenses/sources/bans и cargo-machete pass; desktop feature paths, app/desktop tests, primary workspace check и MSRV 1.92 check подтверждены. Полный handoff: `mem:dependency-security/aud-002-2026-08-23`.

## Current dependency-gate status after S04X (2026-07-20)

- S04X закрыл прежние RUSTSEC-2026-0194/0195 через documented exact-source `wayland-scanner` patch на `quick-xml 0.41`; актуальный `cargo deny check` проходит advisories/bans/licenses/sources. Старые разделы ниже про blocking quick-xml gate — исторический статус до S04X, а не текущий blocker. Полный patch/XML contract: `mem:xml/core` и `mem:dependency-patches/core`.

- Blocking CI definition lives in `.github/workflows/ci.yml`; exact commands are owned only by `scripts/ci-checks.sh`. `scripts/pre-pr-checks.sh` is a compatibility wrapper that invokes `scripts/ci-checks.sh all`.
- Since Session 17, `format-guardrails` also runs `bash -n` for runtime acceptance scripts and `scripts/tests/playback-smoke-self-test.sh`; these checks validate CLI parsing and full current-schema config generation/production parse without GUI. Runtime hardware/media acceptance remains separate. See `mem:testing/playback-smoke`.
- Stable blocking check names are: `Format and guardrails`, `Strict Clippy`, `Documentation`, `Workspace tests (all features)`, `app-egui (no default features)`, and `MSRV (Rust 1.92.0)`.
- Session 06 adds four independent matrix statuses `Dependency patch (cros-libva)`, `Dependency patch (cros-codecs)`, `Dependency patch (symphonia-format-isomp4)`, `Dependency patch (symphonia-codec-aac)`, plus `Dependency patch integration`. Direct jobs run each standalone manifest/lock; integration invokes `scripts/ci-checks.sh dependency-patches`.
- CI uses Ubuntu 24.04, `actions/checkout@v4`, `actions/cache@v4`, exact cache identities by OS/arch/toolchain/check/manifests, locked Cargo commands, and explicit native build packages: clang, libclang-dev, libasound2-dev, libavcodec-dev, libavutil-dev, libgbm-dev, libva-dev, pkg-config.
- Real GPU/VA-API/audio/display acceptance is not a blocking hosted-runner test. `.github/workflows/hardware-acceptance.yml` is manual and targets `[self-hosted, linux, x64, fastiplayer-hardware]`; it invokes the existing `scripts/playback-smoke.sh --mode full` with explicit real fixture paths. Local and workflow acceptance therefore share the same runner and no software stub substitutes for hardware.
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
- GitHub rulesets and `main` protection APIs were rechecked and still return the private-repo-without-Pro HTTP 403 limitation. Full evidence: `docs/history/readiness_report_2026-07-12.md` (historical snapshot, not current readiness).

## Playlist Session 21 dependency inventory (2026-07-16)

- `scripts/ci-checks.sh` владеет exact `WORKSPACE_CRATE_DIRECTORIES` из всех 37 root workspace members для `cargo machete --with-metadata`; четыре standalone patch crates из workspace `exclude` намеренно не попадают в этот recursive audit.
- `scripts/tests/test_dependency_audit_inventory.py` сверяет exact set, uniqueness и disjoint exclusions с root `Cargo.toml`. Это предотвращает повторный пропуск новых crates; policy discovery сейчас 30 tests.
- Session 21 dependency run проверил все 37 crates: licenses/bans/sources и cargo-machete прошли, а общий gate честно FAIL только на прежних `RUSTSEC-2026-0194/0195` (`wayland-scanner 0.31.10 -> quick-xml 0.39.3`). Advisory ignores не добавлялись; foundation остаётся NOT READY.


## Public launch S01: toolchain/native prerequisites и libva header matrix (2026-09-04)

- CI quality jobs больше не полагаются на частично предустановленный GitHub runner tool cache: `Format and guardrails` устанавливает exact Rust 1.96.0 + `rustfmt`, а `Strict Clippy` — exact Rust 1.96.0 + `clippy` через существующий pinned `dtolnay/rust-toolchain` action. Это закрывает подтверждённый failure `cargo-clippy is not installed`.
- `.github/workflows/toolchain-policy.yml` workspace matrix устанавливает exact native inventory, выведенный из manifests/build scripts: `clang libclang-dev libasound2-dev libavcodec-dev libavutil-dev libdrm-dev libgbm-dev libva-dev pkg-config`. CPAL/alsa-sys требует `libasound2-dev`; video-ffmpeg с feature `ffmpeg` требует только libavcodec/libavutil; cros-libva/bindgen и video-vaapi требуют VA-API/DRM/GBM + clang/libclang. Полный FFmpeg binary и неиспользуемые avformat/avfilter/avdevice/swscale/swresample dev packages этому compile job не нужны.
- Реальная cros-libva compatibility проверяется с обеих сторон ABI boundary: существующий Ubuntu 24.04 standalone job утверждает VA-API 1.20 и отсутствие VP9 fields, новый `Dependency patch (cros-libva, VA-API 1.23)` на Ubuntu 26.04 утверждает libva/VA-API 1.23 и наличие обоих fields, затем оба выполняют полный locked standalone crate test/build. Header version assertions fail closed при drift runner image.
- Source-level workflow ratchet находится в `scripts/tests/test_ci_native_prerequisites.py`; его запускает общий guardrail unittest discovery. Size guardrails `app-egui/state.rs` и `web-media-dash/discovery.rs` намеренно остаются задачей S02 и в S01 не менялись.
