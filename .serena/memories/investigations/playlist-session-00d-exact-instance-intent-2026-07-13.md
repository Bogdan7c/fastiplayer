# Session 00D: exact-instance playback intent и единый internal install (2026-07-13)

## Итог

Session 00D завершена PASS поверх `mem:investigations/playlist-session-00c1-staged-media-transaction-2026-07-13`. Playlist/controller/service policy не добавлялась.

## Boundary и linearization

- `PlaybackIntentControl` в `crates/player-core/src/media_install/playback_intent.rs` — player-owned shared owner для latest-only `PlaybackIntentRevision`.
- Public update: `PlayerWorker::update_playback_intent(PlaybackIntentUpdate) -> Result<PlaybackIntentUpdateReceipt, PlayerWorkerSendError>`.
- Outcomes различают `AppliedToStaged`, `AppliedToInstalled { media_instance_id }`, `StaleRevision { latest_revision }`, `UnknownOrSupersededRequest`, `StaleInstance`. Повтор exact revision идемпотентен.
- Update, взявший control mutex до commit owner turn, входит в highest accepted staged intent. Commit удерживает тот же mutex во время infallible ownership switch, затем записывает exact `request_id -> media_instance_id` до публикации `Installed`. Update после barrier адресуется только exact just-installed instance.
- Следующий successful install переводит предыдущую request/instance correlation в stale tombstone; старый request не может управлять новым current media.
- Payload intent update хранится в mutex state. Отдельный capacity-one `playback_intent_wake` лишь будит worker и coalesce-ится при Full, поэтому обычная command queue не блокирует intent control и UI не ждёт I/O.
- Пока новый request staged, matching intent также адресуется exact old current instance. Cancel/failure/supersede до barrier сохраняют последнее пользовательское Playing/Paused, а commit применяет intent новому instance без transient wrong-state start.

## Единый install algorithm

- Все internal `PlayerSession::{load_prepared_media_with_autoplay, load_prepared_media, load_demuxer_with_autoplay, load_demuxer}` проходят stage/prepare/Ready/authorize/atomic commit/Installed.
- Старые destructive helpers `install_prepared_media`, `begin_media_open`, `CompatibilityMediaInstallOutcome` и public `PlayerWorker::load_demuxer` удалены.
- Единственный временный app facade до Session 10D: `PlayerWorker::load_prepared_media`, возвращает typed `MediaInstallReceipt`. Его sender helper `load_prepared_media_compatibility` имеет `pub(super)` visibility и использует тот же strong algorithm.
- Commit-контур вынесен в `crates/player-core/src/session/staged_media_install/commit.rs`; основной staged owner module остаётся меньше 700 строк.

## App handoff Session 10D

Compatibility facade пока вызывают `state/media_jobs.rs::{load_file, load_prepared_local_file, load_prepared_external_media, load_youtube_demuxer}`.

Startup consumers: `startup_media.rs::{StartupMediaController::start_pending_initial_media, poll_direct_media_startup_job, poll_youtube_startup_job}`.

Settings consumer: `frame_prepare/settings_runtime_adapter.rs::FrameSettingsRuntimeAdapter::reconfigure_active_media`. После enqueue он вызывает `AppState::restore_playback_after_media_reconfigure`, где Play/Pause пока не коррелированы с request. Session 10D должна удерживать receipt, провести app-owned Session 00C half через Ready/authorization/Installed/post-Installed pointer commit и заменить restore на revision/exact-instance intent.

## Tests и проверки

Focused tests:
- `crates/player-core/src/media_install/playback_intent/tests.rs`
- `crates/player-core/src/worker/staged_media_install/tests.rs`
- `crates/player-core/src/session/tests/staged_media_install.rs`
- `crates/player-core/src/worker/tests.rs`

PASS:
- 512 player-core tests
- 268 app-egui tests
- 99 render-wgpu-video tests
- 13 video-backend-api tests
- `cargo +1.96.0 check --workspace --locked`
- strict workspace Clippy with `-D warnings`
- fmt, refactor guardrails, git diff check
- Serena reference audit and diagnostics
