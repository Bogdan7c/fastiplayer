# Playlist Session 00A feasibility (2026-07-13)

Status: controlled GO после решения пользователя; production code/API не менялись. Strong D39 сохранён через отдельные Session 00C/00C1, D10e выбран Linux-v1 за platform-neutral adapter boundary. Полный handoff находится в `user/playlist_queue_implementation_plan.md`.

## Доказанная причина D39 NO-GO

Current `PlayerSession::load_prepared_media_with_autoplay` вызывает `begin_media_open`, который destructive `reset_media_state` выполняет до fallible `OpenMedia` dispatch. Reset освобождает demux/audio/output/frames и пытается fallible flush/clear singleton video decoder, логируя ошибки и продолжая.

`PreparedMedia` и lazy audio plan можно stage-ить без old mutation. Video нельзя: `select_default_video_track -> configure_decoder_stream_for_track -> PlaybackPipeline::configure_video_decoder_stream` конфигурирует единственный active decoder handle и сохраняет typed `Unsupported`/`Backpressure`/`Fatal`. Concrete candidate backend и matching WGPU materializer/provider создаются в `app-egui::state::video_backend`, проходят через `video-backend-api`, а не принадлежат `player-core`.

Strong old Playing/Paused preservation возможен с максимум `1 active + 1 staged` resource set: candidate `PreparedMedia`, pure audio plan, заранее сконфигурированный candidate decoder/backend и staged matching materializer. Второй `PlayerSession` не нужен. Пользователь утвердил scope split: Session 00C создаёт cross-crate app/composition candidate pair, Session 00C1 выполняет player atomic transaction; после обновления prompts Session 00B разрешена. Если Linux driver не разрешает временный второй decoder/backend, candidate typed-fail-ится до barrier и old playback сохраняется; destructive fallback запрещён.

## Остальные gates

- D71 identity axes (`TraversalCurrentItemId`, app lineage, exact player instance/binding, pending target) совместимы.
- D72 process-lifetime `PlaylistRuntime` должен жить в `AppShell`; renderer-bound `AppState` только rebind-ится по generation. Resume: install StartPaused -> correlated seek -> restore intent.
- D76 подтверждён для winit 0.30.13: `EventLoop::<AppWakeEvent>::with_user_event().build()`, `create_proxy`, `ApplicationHandler<AppWakeEvent>::user_event`, `send_event -> EventLoopClosed<T>`. Нужен per-owner false->true edge и clear/recheck/re-arm.
- D73 budgets: 100,000 raw entries; 64 MiB native path + compact natural-key payload; target-only/no-prefix overflow. Operation/counting-memory tests, не wall-clock.
- D74: per-direction contiguous terminal frontier; far verified record не получает Item ID/commit/readiness до terminal outcomes nearer keys. Lookahead 256/direction, verified-unadmitted cap 512 total. Shuffle получает только committed-admission reevaluation signal; Previous идёт по factual history.
- Discovery execution proposal: 2..=4 workers, one permanently foreground-reserved, input 256 with 16 foreground-reserved slots, max 16 active jobs, one latest progress and one lossless terminal slot/job.

## D10e

Rust 1.92+ `std::fs::File::try_lock` даёт non-blocking exclusive lock (Linux flock LOCK_EX|LOCK_NB), release при закрытии всех duplicated/inherited descriptors и stable inode при no-unlink. Пользователь выбрал Linux-v1: modes 0700/0600, descriptor metadata regular-file/current-user validation, tightening существующего stable lock artifact до 0600 и explicit non-inheritance subprocess tests. Platform-neutral `AppInstanceLease` facade использует Linux adapter; остальные OS пока возвращают typed `UnsupportedPlatform`, будущие adapters проходят общий conformance suite без rewrite bootstrap/AppShell.

Pinned zbus 5.15 builder всегда использует DoNotQueue, но default также включает AllowReplacement и ReplaceExisting. Future MPRIS callsite должен явно вызвать `.allow_name_replacements(false).replace_existing_names(false)` и typed-map `zbus::Error::NameTaken` в non-fatal MPRIS-disabled без fallback.

## Проверки

- `cargo test -p player-core failed_prepared_media_open_publishes_error_without_resetting_old_playback` PASS: доказывает только adapter failure до destructive player install.
- `cargo test -p player-core playback_pipeline_decoder_boundary_preserves_config_error_states` PASS: подтверждает fallible singleton decoder configure outcomes.


## Progress update 2026-07-13

Session 00C завершена PASS: bounded detached backend/materializer boundary реализована без player ownership switch. Следующая разрешённая session — только 00C1. Детали: `mem:investigations/playlist-session-00c-candidate-video-resources-2026-07-13`.