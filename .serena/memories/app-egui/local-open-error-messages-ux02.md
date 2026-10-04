# Понятные ошибки открытия локального файла (UX edge cases, сессия 02, 2026-10-04)

План: `user/ux-edge-cases/02-local-open-error-messages.md`, итог: `user/ux-edge-cases/results/02.md` (приватно).

## Владельцы и границы
- Классификация (бизнес-решение) — `app-egui/src/media_open/local/failure.rs`: `LocalOpenFailureReason` (Copy, без пути/текста ОС; FileNotFound, AccessDenied, IsDirectory, EmptyFile, UnrecognizedFormat, DamagedOrTruncated, ReadFailed, ReadTimedOut, NoAudioOrVideo, ChangedDuringOpen, InternalError), `LocalOpenFailureOutcome { Cancelled, Failed(reason) }` (отмена — не ошибка), `PrepareLocalOpenError::user_outcome()` и `diagnostic_chain()` (явный обход `Error::source()` для лога: thiserror `Display`/`{:#}` цепочку не печатает). Матчи по `DemuxOpenError`/`DemuxProbeRejection`/`DemuxFactoryOpenError` исчерпывающие (у enum-ов нет `non_exhaustive`) — новый вариант в demux-api потребует явного решения.
- `prepare_local_open`: новые варианты `PrepareLocalOpenError::Directory` (явная stat-проверка до `File::open`: на Linux каталог открывается и падает только при чтении в probe, где `io::ErrorKind` теряется в строке `InputFailure.reason`) и `EmptyFile` (size==0 у открытого handle-а, до probe).
- Тексты — единственный владелец `app-egui/src/local_open_message.rs`: `local_open_failure_message(path, reason)` → «Не удалось открыть «clip.mkv»: файл не найден», `local_open_preparing_message` → «Открываем «clip.mkv»…», `local_open_failure_row_summary(reason)` → «Файл не найден» (бейдж строки). Используют: кнопка Open/замена очереди (`state/media_jobs.rs`), CLI/restored старт (`startup_media/orchestration/drain.rs`), строка плейлиста (`state/playlist_transport.rs`).
- `LocalFileOpenResult::PrepareFailed { path, reason }` — типизированная причина вместо строки; worker пишет техническую цепочку в лог без имени файла. Отмена подготовки → `LocalFileOpenResult::Cancelled`.
- Coordinator: `MediaPreparationFailureKind::LocalOpen(LocalOpenFailureReason)`; поле `MediaOpenTerminalOutcome::PreparationFailed.kind` теперь production (было `#[cfg(test)]`). Intent-методы: `MediaPreparationFailureKind::local_open_failure_reason()`, `MediaOpenTerminalOutcome::local_open_failure_reason()`, `StrongMediaOpenError::local_open_failure_reason()` (отдельный файл `state/strong_media_open/local_failure_reason.rs`, т.к. `strong_media_open.rs` у лимита 800). `LocalSourceChanged` остался отдельным видом → причина ChangedDuringOpen.
- Плейлист: `PlaylistRuntime::report_playlist_navigation_failure(request_id, item_id, PlaylistTargetFailureSummary)`; `Generic` = прежнее поведение (общий текст автоперехода, без бейджа у ручной навигации), `Specific(Arc<str>)` = бейдж с причиной; для ручной навигации — `PlaylistController::record_manual_navigation_failure_reason` (Preparation/Unavailable, снимается успешным install).
- Старт: отмена локальной подготовки (`finish_cancelled_local_preparation`) — без сообщения, без бейджа и без перехода к следующему restored-элементу.

## Логи
- Имя файла в новые/изменённые логи не пишется; `InAppQueueReplacementIntent::Debug` не печатает метку для локального файла (только для URL, host-only). Прежний `warn!(source = %safe_label)` в `media_open/preparation.rs` (filename через `from_local_path`) не менялся.

## Тесты
- Сквозные на реальных файлах через `prepare_local_open`: `local_open_message/tests.rs` (нет файла, chmod 000 с пропуском под root, каталог, пустой, текст-мусор, PNG→.mp4, обрезанный TS/MP4, `/`, non-UTF-8, отмена; родитель `private-parent-dir` не попадает в текст).
- Классификация: `media_open/local/failure/tests.rs`; job: `local_file_open::tests::preparation_job_reports_typed_reason_for_missing_file`; coordinator: `media_open/preparation/tests.rs::missing_local_file_preparation_carries_user_reason_for_playlist_row`; плейлист-бейдж: `playlist_runtime::transport_execution::tests` (manual Specific, automatic Generic).

## Сессия 03 (2026-10-04)
- Тексты расширены причинами отказа player-а: `local_open_failure_message`/`local_open_failure_row_summary` принимают `impl Into<MediaOpenUserFailureReason>` (старые вызовы с `LocalOpenFailureReason` не менялись). Файл `state/strong_media_open/local_failure_reason.rs` переименован в `user_failure_reason.rs`: `StrongMediaOpenError::user_failure_reason()` (бейдж строки) и `user_outcome() -> StrongMediaOpenUserOutcome { Silent, Failed(reason) }` (Busy/Cancelled → Silent, неклассифицированное → InternalError). «worker недоступен» удалён (`AppState::report_prepared_local_install_failure`). Startup: `StartupPendingInstall.local_target: Option<StartupLocalTarget { path, sibling_discovery: StartupSiblingDiscovery }>` + `startup_install_failure_texts`. Детали: `mem:app-egui/media-open-coordinator-s10c` (UX03).

## Вне объёма (другие сессии / бэклог)
- английский текст пересборки после смены настроек (`settings_runtime_adapter`), стартовый pending «Подготовка local media...» без имени.

## Старт (после приёмки)
- `StartupMediaController::handle_preparation_failure` = лог + `publish_preparation_failure(user_message, row_summary)`; локальный старт — `orchestration/drain.rs::handle_local_preparation_failure` (лог только `reason`, бейдж restored-строки = короткая причина). Не логировать текст для пользователя локальных ошибок: он содержит имя файла.
- `diagnostic_chain()` пропускает звенья, текст которых уже встроен в `Display` верхнего уровня.
- Ручная приёмка GUI без риска для данных владельца: `XDG_CONFIG_HOME=<scratch>` изолирует config, instance lock и playlist-state; MPRIS `busctl --user call org.mpris.MediaPlayer2.fastiplayer … Player Next` = путь ручной навигации. Инструментов ввода нет; AccessKit не виден в AT-SPI без включённой доступности.
