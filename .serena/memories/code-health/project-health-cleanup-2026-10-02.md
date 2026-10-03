# Project health cleanup (2026-10-02)

## Паники (см. `mem:panic-invariant-policy`)
- Включены workspace clippy lints unwrap_used/expect_used/panic; `clippy.toml` освобождает тесты; интеграционные test crate-ы объявляют `#![allow(...)]` в корне; составной `cfg(all(test, unix))` переписан как `#[cfg(test)] #[cfg(unix)]`.
- Реальные дефекты исправлены: отравленный барьер snapshot (player-core worker) и mailbox app_wake снимают poison (данные согласованы), FPS-трекер тоже; media-prefetch: `append_chunk` -> `Result<_, PrefetchAppendError::AddressSpaceExhausted>`, worker трактует как EOF, `PrefetchConfig` отклоняет chunk > usize; HDS/FLV читают фиксированные поля массивами (`first_chunk`), HLS/Smooth/DASH убраны проверки-потом-expect (`take_if`, `next_if`, `zip`, enum вместо согласованных Option, `non_zero()` accessors у NonZero-обёрток).
- F4F inventory (`scripts/s42_f4f_guardrail.py`) — owner-approved список функций f4f.rs; новые функции туда не добавлять без владельца.

## Мёртвый код app-egui
- Сняты модульные `allow(dead_code)`; удалено мёртвое (эталон: rustc на HEAD без заглушек, 166 элементов). Неподключённые фичи удалены вместе с тестами: retry ручной навигации, одиночный move_item, media_open_command/report_*/record_request_error, reevaluate_held_ended, deferred transport execution, cancel metadata sort/manual add, video candidate cancel_pre_barrier/backend_resource, worker availability.
- Тестовые входы в живую логику сохранены под `#[cfg(test)]`: on_installed, remove_item/remove_other_items (+runtime), replacement-only подтверждение, наблюдатели view/identity/video candidate; поля только для тестов (`Waiting.wait_id/direction`, `Stop{item_id,media_instance_id,cause}`, `Cancelled.cause`, `FatalInvariant.violation`, `PreparationFailed.kind`, `CoalescePending.request_id`, `AbortedBeforeDispatch.no_item`, строки view snapshot).
- Ошибки больше не теряются молча: fatal media-open violation, discovery batch/submit/draft, Manual Add start, stop cause, guard-исходы ручной навигации/Play item — в лог.
- Исправлен баг: отказ импорта после подтверждения терялся; общее соответствие `PlaylistImportContinueOutcome::failure_feedback()`, тест `confirmed_import_failure_reaches_user_feedback_like_direct_continue`.

## Потеря guard-исходов — подтверждено и исправлено 2026-10-03 (session-00)
- Баг существовал с Session 11B: Play row/Next/MPRIS Stop во время install терялись; в `AwaitingReady` старый request к тому же доигрывал как «внешний» (без item binding). Исполнитель и его тесты восстановлены как production-код; детали — `mem:app-egui/transport-guard-execution-2026-10-03`.

## Раскладка тестов (монолиты разрезаны тематически, логика тестов не менялась)
- Общие фейки/помощники остаются в `tests.rs`, темы — в `tests/<тема>.rs` с `use super::*;` (пути к модулю под тестом — `super::super::`, `include_str!` — на уровень глубже).
- НЕ дробить файлы, на которые ссылаются evidence-каталоги приёмки по точному пути (`service-ytdlp/compatibility/*/roadmap-trace-s42.json`, `tests/final_acceptance_s42`, `cross_provider_integration_s41`, refactor guardrail): `player-core/src/session/tick/tests.rs`, `service-ytdlp/src/candidate/tests.rs` оставлены монолитами.
- player-core `session/tests/seek_completion/{commit_gates,preroll_backpressure,output_floor_freeze,wakeup_resume}`, `pipeline/tests/{queues_media_slots,audio_boundaries,clock_mapping,demux_timing,backlog_recovery,seek_reset}`, `worker/tests/{shutdown,media_install,runtime_settings,commands,wakeup}`.
- app-egui `settings_runtime/tests/{snapshot_routes,dynamic_options,preview,transaction_apply,sidebar_resize}`; symphonia-demux `symphonia_demuxer/tests/{open_metadata,seek_modes,decode_point_before,events_errors}`; demux-api `progressive/tests/{readiness,worker_lifecycle,stale_seek,receipted_seek}`; config `store/tests/{schema_migration,render_validation,persistence_sections,frame_server,ui_and_legacy,field_validation}`.
- `player-core/src/session/tests/test_support.rs` (2.1k) — общая инфраструктура, не дробилась.

## План выноса web-media
- `user/web-media-extraction/plan.md` (приватно, сессии-промпты там же, включая session-00 по багу guard-исходов): 3 волны (листья ~1.75k → opener-ы/orchestration ~7.6k → startup native jobs ~3k с портами). Session-01 выполнена 2026-10-03 → `mem:media-source-open/core`.

## Прочее
- `docs/benchmarks/thinkpad-t480s*.json` удалены из дерева, ссылки на commit 6c2b670f.
