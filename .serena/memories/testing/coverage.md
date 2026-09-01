# Stable coverage v2 (authoritative, 2026-09-01)

Этот документ полностью заменяет прежние Session 07B/28/S42 утверждения об aggregate v1 baseline и одном measured run. Историческая модель сохранена только внутри `coverage/baseline.json.legacy_report_only` как provenance и никогда не участвует в blocking decision.

## Владельцы и tracked policy

- `scripts/coverage.sh` — единственный shell entrypoint: `check` проверяет tracked v2 baseline, `bootstrap <proposal>` создаёт review-only proposal и не меняет tracked policy.
- `scripts/coverage_stability.py` + `scripts/coverage_stability_schema.py` владеют source-coordinate run/cohort/baseline schemas, stable ratchet, atomic baseline-update policy и measurement-exception lifecycle.
- `coverage/baseline.json` — schema v2. Текущий G2 tracked baseline квалифицирован exact 9/9 intersection трёх независимых cohort-ов на одной source revision `3f9d5f90`; raw SHA-256 `3c04e5d97e7d806dc05f481b4f536ebbc7935861d898ae7f06ffe1ab88d5050a`, logical `sha256:4295f9a05fb06ba6a11d04d5623d154c371c758224f6a88e84f7938067267afb`. Cohort hashes: `sha256:18b816b0067f5f4eda21600cbd2b8852d95e2f484a102eac8aadb2075d7ab875`, `sha256:be8657278eb85c32148f703f61045616ee87b2eae914b64468abf97308d00e6e`, `sha256:ae578adf3c0c3aa9e0365d31f8ebb1eee59a6975b9f75489290fbe26016cc7b1`. Atomic transition прошёл с неизменным пустым measurement-exception ledger и подтверждён двумя fresh `scripts/coverage.sh check` с пустыми `regressions`/`universe_changes`; оба дали check hash `sha256:92e76ae086a9b6ae8a2820411032d0231b93d00e4a5814691341613d5eeed6be`.
- `coverage/measurement-exceptions.json` — единственный blocking exception ledger, schema v1; initial exact empty ledger SHA-256 `1f64ad40d0db9ebf1a108da65cd02c8baec6a26c41e78e85add972c6f3534a2b`.
- `coverage/exceptions.json` и embedded v1 baseline — frozen `legacy_report_only` provenance. Они не разрешают v2 regression и не являются параллельным источником истины.
- `coverage/policy.json` по-прежнему классифицирует blocking/informational crates. `coverage/executable-inventory-policy.json` типизированно разрешает runtime-built root только для `settings-derive/tests/trybuild`.

## Методология

1. Один `cargo test --no-run` materializes parent executable cohort.
2. Exact stale runtime root transactionally quarantined. Typed cargo-test prewarm materializes trybuild; prewarm profiles удаляются.
3. Source tree, parent executable inventory и runtime trybuild inventory freeze-ятся до measured run 1. Build/runtime semantic identity — logical path + mode + size + SHA-256; symlink/root escape/add/remove/mode/content change fail closed. Parent metadata optimization может пропускать повторный hash только после runtime ctime probe; mtime/inode сами по себе не semantic.
4. Ровно три measured workspace runs выполняются с обычной concurrency. Source coordinate классифицируется: 3/3 — stable; 1/3 или 2/3 — variable diagnostic; 0/3 — uncovered. Blocking ratchet использует stable coordinates, а variable remains visible report-only.
5. Run/cohort/artifact publication transactional. Build, runner или publication failure восстанавливает предыдущие merge metadata и quarantined runtime root; partial stage/prewarm/run1/run2 profiles не публикуются. После успешной публикации полного cohort semantic ratchet выполняется отдельно: его exit 1 намеренно сохраняет новый cohort, variable diagnostics и `check.json` как evidence регрессии, но не меняет tracked baseline/exception ledger и не считается успешным gate. Successful cohort retains exact authoritative run3 profraw set + profdata/list for report artifact.
6. Runtime builder policy, source identity, toolchain identity, build/runtime inventories and every published artifact are recorded in schema-v2 cohort manifest. New runtime owner fails until explicitly modeled.

Текущий independently qualified G2 baseline принят на Rust 1.96.0, LLVM 22.1.2 и cargo-llvm-cov 0.8.7; source universe hash `sha256:d1f4228c0ef44e817b64e2fb5e754ce944e0581afd6a5c7f302e5ecb7142be3b`. Его exact workspace stable intersection: functions 15,652/19,879, lines 162,784/210,449, regions 204,329/267,594; blocking stable: functions 9,846/11,681, lines 99,471/115,753, regions 124,744/148,339.

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
