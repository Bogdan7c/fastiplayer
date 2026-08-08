# Staged media install: settings reconfigure fence (2026-08-05)

Дополняет `mem:investigations/playlist-session-00c1-staged-media-transaction-2026-07-13` и `mem:settings-ui/application-contract-s08`.

## Инвариант

Настройки video backend не должны перестраивать runtime, пока strong media open или suspended resume владеют staged media transaction. Настройки в этот момент получают retryable `PlayerRuntimeApplyError::RuntimeBusy(PlayerRuntimeBoundaryActivity::PipelineLifecycle)`; staged open не отменяется автоматически.

Fence двухуровневый:

- `AppState::runtime_reconfigure_boundary_activity` сначала проверяет `has_pending_prepared_media_strong()` и `has_pending_suspended_media_resume()`, а уже затем спрашивает worker. Это закрывает pre-player окно, когда app уже владеет подготовленным/suspended candidate, но worker ещё не видит staged install.
- `PlayerSession::runtime_reconfigure_boundary_activity` считает `has_staged_media_install()` активностью `PipelineLifecycle`. Это закрывает worker-side TOCTOU и действует на стадиях `Pending` и `ReadyToCommit`.

После terminal outcome staged install boundary освобождается; отсутствие staged transaction снова даёт обычную классификацию runtime activity.

## Functional coverage

`crates/player-core/src/session/tests/staged_media_install.rs` проверяет реальный settings boundary через `install_video_backend_with_intent(..., VideoBackendInstallIntent::SettingsReconfigure)`:

- Pending preflight из-за временной недоступности ресурса => `RuntimeBusy(PipelineLifecycle)`.
- ReadyToCommit => тот же typed busy.
- После Installed и очистки staged owner => boundary возвращает `None`.

`crates/app-egui/src/state/tests.rs` содержит архитектурный guard порядка app-side checks, потому что полноценный `AppState` не имеет headless-конструктора и требует настоящие Window/audio worker.

## Проверки

- `cargo test -p player-core session::tests::staged_media_install`
- `cargo test -p player-core runtime_pipeline_reconfigure`
- `cargo test -p app-egui app_runtime_reconfigure_boundary_checks_pre_player_media_lifecycle_first`
- `cargo fmt --all -- --check`
- `git diff --check`
