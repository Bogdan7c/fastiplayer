# Coverage baseline and ratchet (Session 07B, 2026-07-10)

## S42 current override (2026-07-25)

- Этот раздел supersedes все более старые утверждения ниже, что coverage `NOT READY`, baseline/exceptions не обновлены или `scripts/coverage.sh check` неизбежно падает. Исторические Session 28/playlist записи сохранены только как причина текущей policy.
- Final conservative per-crate baseline envelope on Rust 1.96.0 / cargo-llvm-cov 0.8.7 contains the exact inventory of 47 blocking and 11 informational crates. Workspace floor: lines 135834/181804, functions 13197/17245, regions 169757/228313. Blocking group floor: lines 83276/99646, functions 8338/10114, regions 103632/125867.
- Latest final clean gate artifact was higher: workspace lines 135842/181804, functions 13200/17245, regions 169766/228313; blocking group lines 83284/99646, functions 8341/10114, regions 103641/125867. Scheduler-dependent lifecycle tests can swap a few execution counters between neighboring async paths, so baseline aggregates are sums of per-crate minima actually observed across clean runs, not invented global thresholds.
- Owner-approved one-time S42 rebaseline still contains exactly 28 exact `scope/metric` exceptions with previous/allowed counters, concrete reason/follow-up and `review_by = 2026-10-25`; no new exception was added for scheduler stabilization. Future regression cannot reuse a row because counters must match exactly.
- `validate-baseline` runs before LLVM and checks policy/baseline inventory plus full exception lifecycle: versioned exact schema, counter bounds, nonempty reason/follow-up, non-expired review date and unique `(scope, metric)`. `check-baseline-update` separately binds actual previous→proposed decreases to exact exceptions. Raw LCOV execution counters with the top `u64` bit set are rejected as corruption before baseline/report publication.
- Final `scripts/coverage.sh check` passed inside `scripts/final-acceptance.sh`; manual URL/hardware acceptance is independent and remains `NOT RUN`.

- Standard tool is exact `cargo-llvm-cov 0.8.7` plus primary-toolchain `llvm-tools-preview`. Policy/classification lives in `coverage/policy.json`; compact versioned counters live in `coverage/baseline.json`; bounded exceptions live in `coverage/exceptions.json`.
- Local/CI entrypoint is `scripts/coverage.sh check`. It always runs `cargo llvm-cov clean --workspace`, then the hermetic `--workspace --all-features --locked --no-fail-fast` suite once, and emits summary JSON, LCOV and HTML report-only artifacts.
- Blocking ratchet compares exact integer fractions (covered/total), never rounded percentages, for lines/functions/regions at three levels: first-party workspace, aggregate pure contract/business group, and every pure crate. Any decrease fails.
- Hardware/FFI/UI-shell crates are separately listed as informational. Their per-crate metrics do not block until the path becomes hermetic, but their measured files remain part of the workspace aggregate ratchet.
- PR baseline changes are compared with the target branch. Every decreased `scope/metric` requires an exact non-expired exception containing previous/allowed counters, reason, review date and bounded follow-up. A baseline edit without it fails.
- No source exclusions currently exist. Generated cros-libva raw bindings are in a non-workspace patch crate; build scripts are not included; manual hardware/runtime paths remain visible as informational coverage.
- CI check name is `Coverage ratchet`; artifact `coverage-report` contains `target/coverage/` plus raw `*.profraw`/`*.profdata` from `target/llvm-cov-target`.
- Human documentation is `docs/code-coverage.md`; focused policy tests are `scripts/tests/test_coverage_metrics.py`.
- Initial line baseline: workspace 58,981/81,342 (72.5099%); pure blocking group 36,977/43,992 (84.0539%). Low line-coverage owners visible in initial map include `service-direct-media` 370/489, `settings-derive` 702/899, and informational `desktop-integration` 303/818, `render-wgpu-shell` 200/496, `app-egui` 5,944/11,318.

## Session 28 readiness audit (2026-07-12)

- `scripts/coverage.sh check` currently fails after Sessions 22–27E: workspace lines 58,981/81,342 -> 57,050/80,520; blocking-group lines 36,977/43,992 -> 34,353/41,510, with functions/regions also decreased.
- Root cause is metric instability under behavior-neutral test relocation: cargo-llvm-cov default filename regex excludes separate `tests/`, `tests.rs`, and `*_tests.rs`, while the versioned baseline counted the same tests when they were inline inside production files. Tests still execute; the ratchet correctly refuses an unexplained baseline decrease.
- Do not run `scripts/coverage.sh baseline` as a shortcut. A separate policy package must choose stable test-code classification, add inline-vs-external characterization tests, and migrate every decreased scope through exact non-expired exceptions. Evidence and prompt: root `readiness_report_2026-07-12.md` and `user/session_28_followup_coverage_baseline_after_decomposition_2026-07-12.md`.


## Workspace inventory guardrail (Session 00 playlist baseline, 2026-07-13)

- `ui-artwork-egui` явно классифицирован в `coverage/policy.json` как informational UI surface: его отдельная crate-метрика не blocking, но production/tests остаются в общем workspace ratchet.
- `scripts/tests/test_coverage_metrics.py::CoveragePolicyInventoryTests::test_every_workspace_crate_is_classified_by_coverage_policy` fail-fast сверяет каталоги всех root workspace members с точным объединением `blocking_crates` и `informational_crates`. Поэтому новый workspace crate без осознанной coverage-классификации должен падать уже в `scripts/ci-checks.sh format-guardrails`, до дорогого `scripts/coverage.sh check`.
- `coverage_metrics.py` намеренно продолжает fail-closed отклонять LLVM source crate, отсутствующий в policy; launcher `scripts/coverage.sh` и baseline/exception semantics не ослаблялись.
- После классификации `ui-artwork-egui` current `scripts/coverage.sh check` снова доходит до известного Session 28 relocation/ratchet failure; clean suite и report generation проходят. Current aggregate counters на commit `fa7511b`: workspace lines 57,953/81,391, functions 5,794/7,830, regions 71,331/100,421; blocking group lines 34,509/41,733, functions 3,553/4,181, regions 42,142/51,096. Baseline/exceptions не обновлялись.

## Playlist Session 21 inventory hardening (2026-07-16)

- `bounded-work-executor`, `natural-sort-key`, `playlist-core`, `playlist-discovery` и `playlist-state` теперь явно входят в `blocking_crates`. Это исправляет coverage inventory, а не baseline: `coverage/baseline.json` и `coverage/exceptions.json` не менялись.
- `scripts/coverage.sh check` снова выполнил clean suite/report generation и честно остановился на известном D28 relocation/ratchet. Current workspace: lines 81,769/115,516, functions 8,155/11,035, regions 100,373/142,282. Current blocking group: lines 45,412/55,041, functions 4,703/5,660, regions 55,182/67,412. Baseline остаётся workspace 58,981/81,342 lines и blocking 36,977/43,992 lines.
- Нельзя называть этот run PASS или менять baseline/exceptions как часть playlist Session 21. Feature-scope regression suite зелёная, repository foundation по coverage остаётся NOT READY.
