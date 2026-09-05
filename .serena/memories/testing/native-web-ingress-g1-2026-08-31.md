# Native web ingress G1 qualification (2026-08-31)

## Outcome

- Gate-only session завершила accumulated N01–N05B foundation без feature logic. Neutral core purity, config schema v10 single source of truth, YtDlp DTO isolation behind extractor adapters, durable secret-safe source intent и exact typed process reasons прошли self-review.
- Process-spy parity доказана отдельно: `service-ytdlp::invocation::tests` page/extractor fixtures проходят через injected launcher и реально spawn-ят controlled children; `app-egui::native_direct_fixture_cannot_reach_extractor_launcher` подтверждает zero spawn для native direct ingress.
- Финальные gates PASS: fmt/diff, focused web-media-core/config/fastiplayer-settings/service-ytdlp/app no-default suites, strict affected Clippy, workspace all-targets/all-features check+tests, rustdoc, `scripts/pre-pr-checks.sh`, refactor/S42 guardrails, release workspace all-features build и Serena references/diagnostics audit.

## Gate-only corrections

- Исправлены stale S41/S42 evidence anchors и schema-v10 playback-smoke self-test; refactor guardrail теперь привязан к provider-neutral media-open boundary.
- Module-size enforcement восстановлен переносом только уже существующей логики: app startup direct/native-HLS composition -> `startup_media/orchestration/web_preparation.rs`; settings media-service route composition -> `routing/media_service.rs`; process-tree tests -> `process_tree/tests.rs`. Production ownership/API semantics не изменились.
- `source-core::abortable_http_task` test polling больше не зависит от immediate completion до первого poll; public worker-stopped diagnostic покрыт детерминированно.
- Config/web-selection boundary tests закрепляют rejection без partial mutation, public selection shape и exact error/source semantics.
- `suppaftp` lockfile обновлён 10.0.1 -> 10.0.2 для устранения RUSTSEC-2026-0271; manifests/API не менялись.

## Coverage

- Из-за variable/concurrency evidence выполнены три независимых cohort-а, всего 9 measured runs на одной source revision, и exact 9/9 intersection с file-local audit.
- Atomic baseline update PASS без exceptions. Logical baseline hash: `sha256:8d6242e05724c8ccaa0d9bd118aa8b059de3f5ab1353491806493d9b4ef0b010`; raw file SHA-256: `d6bd75a3e8ded589fd4e8e9b8e9461ea1b541c92d4f22c07369849b23fd92fd2`.
- Workspace stable: functions 15,281/19,612; lines 157,433/205,843; regions 198,245/262,103. Blocking stable: functions 9,765/11,653; lines 98,353/115,061; regions 123,377/147,538.
- Два fresh post-install `scripts/coverage.sh check` завершились PASS с `regressions=[]` и `universe_changes=[]`. Exception ledger остался exact empty. Полная policy: `mem:testing/coverage`.

## Handoff

- G1 commit/push должен содержать только qualification, test, evidence, dependency-security, generated coverage baseline и memory updates.
- Следующая feature session N06 запрещена без отдельного разрешения пользователя.
