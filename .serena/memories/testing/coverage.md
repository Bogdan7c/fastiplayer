# Stable coverage v2 (authoritative, 2026-09-01)

Этот документ полностью заменяет прежние Session 07B/28/S42 утверждения об aggregate v1 baseline и одном measured run. Историческая модель сохранена только внутри `coverage/baseline.json.legacy_report_only` как provenance и никогда не участвует в blocking decision.

## Владельцы и tracked policy

- `scripts/coverage.sh` — единственный shell entrypoint: `check` проверяет tracked v2 baseline, `bootstrap <proposal>` создаёт review-only proposal и не меняет tracked policy.
- `scripts/coverage_stability.py` + `scripts/coverage_stability_schema.py` владеют source-coordinate run/cohort/baseline schemas, stable ratchet, atomic baseline-update policy и measurement-exception lifecycle.
- `coverage/baseline.json` — schema v2. Текущий G3 tracked baseline квалифицирован exact 9/9 intersection трёх независимых cohort-ов на source revision `d61a2d87`; raw SHA-256 `8c98f6acb996d9520b58703d29efb3f150bd8ba2cb60813610f1edd4936cf67b`, logical `sha256:ff51d2799a3562816de9a5f919bedb5594dc96c93b772e6e9d45c9f94b7f9743`, source-files hash `sha256:30790a092145e379c73aa0c990a2c7aa6f2a9480287f649b71698f05bd3a7383`. Cohort hashes: `sha256:404996c890975fe666573751a60c86ec05c019cbf0f42be73ab7da4c3611d0d7`, `sha256:8f275aaa75182eba4d42ac12328edd62e4d6a56a9f233d60c974f38b6b07d54f`, `sha256:2b1418916b92d1ae0b595d60bb891d4ddd0d9d8aa429b9b0a8ec788869a2ac3`. Два fresh `scripts/coverage.sh check` прошли с пустыми `regressions`/`universe_changes`; последний cohort hash `sha256:0f63da3c8df0e721c28da36f80f74be2c894658dd4e500418e487f945b5d400b`.
- `coverage/measurement-exceptions.json` — единственный blocking exception ledger, schema v1; G3 ledger raw SHA-256 `86fd10ac3fa54331ec552c98ae82fb59d3982363fc2e67c012fd30608d872c97`. Он содержит одну exact bounded transition row `crate:web-media-adaptive/regions` 2890/3239 → 2904/3255 для 16 новых exposed-prefix regions (14 stable 9/9), review_by 2026-12-01; same-universe loss не разрешает.
- `coverage/exceptions.json` и embedded v1 baseline — frozen `legacy_report_only` provenance. Они не разрешают v2 regression и не являются параллельным источником истины.
- `coverage/policy.json` по-прежнему классифицирует blocking/informational crates. `coverage/executable-inventory-policy.json` типизированно разрешает runtime-built root только для `settings-derive/tests/trybuild`.

## Методология

1. Один `cargo test --no-run` materializes parent executable cohort.
2. Exact stale runtime root transactionally quarantined. Typed cargo-test prewarm materializes trybuild; prewarm profiles удаляются.
3. Source tree, parent executable inventory и runtime trybuild inventory freeze-ятся до measured run 1. Build/runtime semantic identity — logical path + mode + size + SHA-256; symlink/root escape/add/remove/mode/content change fail closed. Parent metadata optimization может пропускать повторный hash только после runtime ctime probe; mtime/inode сами по себе не semantic.
4. Ровно три measured workspace runs выполняются с обычной concurrency. Source coordinate классифицируется: 3/3 — stable; 1/3 или 2/3 — variable diagnostic; 0/3 — uncovered. Blocking ratchet использует stable coordinates, а variable remains visible report-only.
5. Run/cohort/artifact publication transactional. Build, runner или publication failure восстанавливает предыдущие merge metadata и quarantined runtime root; partial stage/prewarm/run1/run2 profiles не публикуются. После успешной публикации полного cohort semantic ratchet выполняется отдельно: его exit 1 намеренно сохраняет новый cohort, variable diagnostics и `check.json` как evidence регрессии, но не меняет tracked baseline/exception ledger и не считается успешным gate. Successful cohort retains exact authoritative run3 profraw set + profdata/list for report artifact.
6. Runtime builder policy, source identity, toolchain identity, build/runtime inventories and every published artifact are recorded in schema-v2 cohort manifest. New runtime owner fails until explicitly modeled.

Текущий independently qualified G3 baseline принят на Rust 1.96.0, LLVM 22.1.2 и cargo-llvm-cov 0.8.7; source-files hash `sha256:30790a092145e379c73aa0c990a2c7aa6f2a9480287f649b71698f05bd3a7383`. Его exact workspace stable intersection: functions 15,696/19,914, lines 163,462/211,068, regions 205,271/268,471. G3 file-local audit 58 changed Rust files не нашёл падения stable count живого кода; 18/18→17/17 в HLS discovery — удалённая iterator closure, заменённая обычным loop. Scheduler-sensitive coordinates playlist worker disconnect, pre-cancelled preparation и playback-intent wake стабилизированы functional production/consumer regressions; stale mismatched progressive seek oracle устранил повторяемый LCOV derived `-1` integrity failure. Полный audit: `mem:testing/native-web-ingress-g3-2026-09-02`.

Для baseline update после конкурентных test-only правок один cohort статистически недостаточен. Обязательный human-reviewed qualification workflow: три независимых cohort-а на одной source revision (9 measured workspace runs), exact 9-run stable intersection, file-local audit каждого изменённого файла и два свежих обычных repeatability-check после установки tracked baseline. Aggregate workspace ratio не может скрывать file-local stable loss. CLI пока не автоматизирует cross-cohort reducer; evidence хранится вне tracked policy и сверяется вручную.

Внутри каждого cohort executable logical paths/mode/size/SHA-256 обязаны быть byte-identical. Между независимыми cohort-ами ELF SHA может различаться из-за linker/compiler nondeterminism только если source/tool/profile и coordinate universes совпадают, а каждый cohort отдельно проходит fail-closed manifest validation. Это осознанная граница измерения, не гарантия reproducible build. Реальный test/build failure остаётся failure: retry, measurement exception или ослабление baseline его не легализуют.

G2 qualification поймала две реальные scheduler-sensitive fixture race до установки baseline: `playlist-state::resume::worker` полагался на случайный disconnect wake sender, а app dynamic-options shutdown не синхронизировал active+retired admission и idle fallthrough. Regression commits `169ade5c` и `3f9d5f90` добавили explicit join/started-call/idle lifecycle oracles; production semantics не менялись. После каждого concurrency-sensitive изменения qualification начиналась заново на одной source revision; неуспешные cohorts/proposals не смешивались с финальными evidence.

## Baseline update policy

PR comparison is one atomic pair:
`python3 scripts/coverage_stability.py check-baseline-update --previous-baseline ... --previous-measurement-exceptions ... --proposed-baseline ... --proposed-measurement-exceptions ...`.

All four inputs are required; command is read-only. Exit 0 = allowed, 1 = well-formed semantic policy violation, 2 = malformed/schema/redaction/I/O failure. Same-universe stable loss cannot be excepted. Cross-universe changes require exact proposed bounded rows; previous rows do not authorize a new decrease. Unknown, stale, overbroad and malformed rows fail closed. Recovery from an expired previous ledger is allowed only by a clean proposed pair, so policy cannot deadlock.

Legacy `coverage_metrics.py check-baseline-update` was deleted; v1 report-only helpers remain for provenance validation only.

## Commands and tests

- Full local/CI gate: `scripts/coverage.sh check`.
- Review-only proposal: `scripts/coverage.sh bootstrap target/coverage/<proposal>.json`.
- Baseline/ledger validation: `python3 scripts/coverage_stability.py validate --kind baseline|measurement-exceptions --input <path>`.
- Focused tests: `scripts/tests/test_coverage_baseline_update.py`, `test_coverage_stability.py`, `test_coverage_metrics.py`, runner/inventory/quarantine/publication suites.
- CI wiring oracle: `scripts/tests/test_s42_release_runner.py::S42ReleaseRunnerTests::test_coverage_check_composes_stable_preflight_suite_and_ratchet`; pure canonical workflow parser: `scripts/tests/coverage_workflow_contract.py`.
- Human contract: `docs/code-coverage.md`; CI overview: `docs/continuous-integration.md`.
