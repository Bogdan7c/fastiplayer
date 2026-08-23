# AUD-011/AUD-012: queue continuation и exact seek identity (2026-08-23)

## Итог

- Отдельная read-only сессия на HEAD `acf7b93d1f4e6de534eaac56b976e79733e82782` независимо и исполняемо подтвердила оба P1 дефекта в production player-session и queue/controller runtime seams. Временные regressions жили только в `/tmp`; рабочее дерево было clean до/после.
- AUD-011 и AUD-012 исправлены и закрыты 2026-08-23. Исходный отчёт обновлён в `user/project_health_audit_2026-08-22.md`.

## AUD-011 ownership boundary

- До player admission exact origin/continuation остаётся внутри `PlannedPlaylistInstall`; app не должен заменять plan одним `item_id`.
- `PlaylistController::report_unstaged_planned_target_failure` потребляет exact plan и маршрутизирует по `PlaylistInstallMutation`:
  - `AutomaticTraversal` проходит общий automatic failure tail;
  - `ManualNavigation` и `Reserved` сохраняют прежнюю manual D55 semantics.
- Общий automatic tail сохраняет opaque `AutomaticTraversalPlan`, attempted set, skip budget, loop guard, error policy и typed `AllCandidatesFailed { attempted_count }` как для pre-request, так и для post-request failure.
- `PlaylistRuntime::report_unstaged_planned_playlist_navigation_failure` возвращает typed `UnstagedPlannedTargetFailureOutcome::{RuntimeUnavailable, Manual, Stopped, OpenItem}`, а не `bool`/`Option`.
- `begin_playlist_source_media_strong` при ошибке до staging возвращает exact plan в `UnstagedPlaylistMediaOpenError`; plan boxed только на failure path для компактного Result.
- `AppState::begin_planned_playlist_install` продолжает синхронно битую цепочку обычным bounded loop, поэтому длинная очередь не создаёт рекурсивный stack growth.
- Functional regressions: `crates/app-egui/src/playlist_runtime/transport_execution_audit_regressions.rs`:
  - `unstaged_automatic_failure_preserves_fixed_continuation_to_c`;
  - `removed_automatic_target_still_advances_fixed_continuation_to_c`;
  - `repeated_unstaged_failures_keep_bounded_skip_budget_and_stop_after_all_candidates` (exact attempted_count 2 для B/C).

## AUD-012 identity boundary

- Каждый variant public `player_core::ExactTimelineSeekOutcome` теперь содержит exact `MediaInstanceId` исходного request. `ExactTimelineSeekReceipt` также хранит binding и открывает его через `media_instance_id()`.
- Player обязан сохранять request identity во всех terminal paths: Applied, InvalidRange, BeyondEnd, StaleInstance, NotSeekable, Expired и Failed.
- App receipt drain возвращает отдельный `Vec<RelativeBeyondEndNavigation>`; несколько outcomes не схлопываются в безымянный bool.
- Authoritative stale fence находится в `PlaylistRuntime::request_relative_beyond_end_navigation`, потому что runtime/controller владеет active queue/media identity. Outcome A при active B возвращает typed `StaleInstance` и не вызывает queue navigation.
- В одном poll первый matching outcome создаёт один Next; повтор того же instance явно coalesce-ится, другой instance диагностируется как stale.
- Player regression: `crates/player-core/src/session/tests/timeline_seek.rs::delayed_beyond_end_keeps_origin_media_identity_after_replacement`.
- Queue regression: `crates/app-egui/src/playlist_runtime/transport_execution_audit_regressions.rs::delayed_beyond_end_from_a_is_stale_after_b_installed_and_c_remains_unopened`.

## Проверки

- `cargo +1.96.0 test -p player-core --locked`: 644/644 PASS.
- `cargo +1.96.0 test -p app-egui --no-default-features --locked`: 959/959 PASS.
- `cargo +1.96.0 clippy -p player-core -p app-egui --no-default-features --all-targets --locked -- -D warnings`: PASS.
- `cargo +1.96.0 check --workspace --locked`: PASS.
- `cargo +1.96.0 fmt --all -- --check`, `python3 scripts/check-refactor-guardrails.py`, `git diff --check`: PASS.
- Serena diagnostics по изменённым source/test modules: без warnings/errors.

## Ограничение проверки

Полноценный GUI/GPU smoke не запускался: обе причинные цепочки находятся до renderer boundary. Независимая сессия исполнила реальные player session и production queue/controller runtime boundaries, включая exact Installed A/B и selection C.