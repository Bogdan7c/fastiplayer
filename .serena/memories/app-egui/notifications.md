# Единый владелец уведомлений (UX edge cases, сессия 04, 2026-10-05)

План: `user/ux-edge-cases/04-notifications-error-lifecycle.md`, итог: `user/ux-edge-cases/results/04.md`.

## Решения владельца
- Вид: фатальная ошибка текущего media — в центре (плашка с ×), всё остальное — toast-ы в правом нижнем углу центральной области (над панелью управления, без sidebar-а).
- Временные — 5 с, информационные — 8 с, у каждого ×; клик по видео ничего не закрывает.
- Стопка до 3 toast-ов, новый первым; одинаковое сообщение (тот же вид+текст) не дублируется, а продлевает срок (id и момент появления сохраняются).
- Фатальная ошибка не истекает: снимается ×, успешной установкой media (`clear_startup_status` в `media_jobs::record_installed_media_observables`) или началом нового открытия (`set_startup_pending`).

## Владельцы и границы
- `state/notifications.rs` — `NotificationCenter` (поле `AppState::notifications`, приватное; заменило `pub startup_error`). Время только инжектируется (`now: Instant`), системные часы внутри не читаются. API: `notify_transient`, `notify_info`, `show_media_failure(msg, MediaFailureOrigin::{MediaOpen, PlayerFailed})`, `resolve_media_failure`, `dismiss(id) -> NotificationDismissOutcome::{Dismissed, AlreadyGone}`, `frame(OpenProgress::{Idle, InProgress(&str)}, UiMotion, now) -> NotificationsFrame { center: Option<CenterNotice::{Progress, MediaFailure}>, toasts, motion }`, `next_wake_deadline()`. Константы `TRANSIENT_NOTIFICATION_LIFETIME`, `INFO_NOTIFICATION_LIFETIME`, `MAX_VISIBLE_TOASTS`, `OPEN_STILL_IN_PROGRESS_MESSAGE`.
- Правило центра: идущее открытие (`startup_pending`) важнее старой ошибки; новая ошибка открытия сама снимает pending у shell-а.
- `state/notifications/player_feed.rs` — перевод фактов player-а: `PlayerEvent::RecoverableError` → временный toast (`record_player_event`); фатальность — только `PlaybackState::Failed` из snapshot-а (`observe_player_snapshot`, edge-detection по тексту, закрытая × ошибка не всплывает на тех же кадрах; выход из `Failed` снимает только ошибку origin `PlayerFailed`). Старый `last_error` без `Failed` больше ничего не показывает. player-core не менялся.
- `state/notification_routing.rs` — glue `AppState`: `notify_transient`, `notify_info`, `notify_open_still_in_progress` (intent-API для сессий 05/07/10/15), `handle_notification_player_event` (из `frame_prepare::record_worker_events`), `notifications_frame`, `apply_notification_actions`, `next_notification_wake_deadline`.
- `ui/notifications.rs` — только отрисовка стандартными виджетами (`Frame::popup`, `Label::wrap`, `small_button("×")`), typed `NotificationAction::Dismiss(id)` через `NotificationUiOutput`. Toast-ы — `egui::Area` `Order::Foreground`, `pivot(RIGHT_BOTTOM)`, `fade_in(false)`: встроенное проявление Area игнорирует reduced motion; единственный владелец анимации — `toast_opacity(age, UiMotion)` (150 мс EaseOutCubic, Reduced → сразу 1.0).

## Важное предупреждение до × (сессия 05, 2026-10-05)
- `ToastKind::Warning` (`lifetime() -> None`, `expires_at: Option<Instant>`): не истекает, не будит окно (`next_wake_deadline` его пропускает), рисуется `error_fg_color`. API: `NotificationCenter::notify_until_dismissed`, конструктор `with_startup_messages(media_open_failure, config_warning, now)` (заменил `with_media_open_failure`). Переполнение стопки вытесняет сначала самые старые toast-ы с таймером, предупреждения — последними.
- Предупреждение о config-е: `app-egui::config_startup_notice::config_startup_warning(&LoadedConfig)` (чистый перевод typed итога в русский текст без путей) → `AppShell.pending_config_warning` → `.take()` в `AppStateStartupContext::new(.., config_warning)` первого `AppState` (после suspend повторно не показывается).
- «Применено, но не сохранится»: `SettingsRuntimeReconfigureHost::report_settings_kept_for_session_only` (default no-op) вызывается из `SettingsRuntimeApplyDelegate::persist` при `PersistOutcome::SkippedSessionOnly`; `FrameSettingsRuntimeAdapter` показывает transient toast `SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE`, статус окна настроек — тот же текст.
- Сквозной тест: `render_tests::broken_config_on_disk_reaches_painted_warning_that_stays_until_closed`; settings: `settings_runtime/tests/session_only_persistence.rs`.

## Пропуски в очереди (сессия 07, 2026-10-05)
- `AppState::show_playlist_queue_notices(Vec<AutomaticQueueNotice>)` (в `notification_routing.rs`) — маршрут итогов skip-цепочки; формулировки — `crate::playlist_skip_message::playlist_queue_notice_delivery` (по образцу `local_open_message`). Детали владельца — `mem:app-egui/playlist-controller-s20`.

## Звуковое устройство (сессия 10, 2026-10-06)
- `player_feed::record_player_event`: `PlayerEvent::AudioOutputSwitchedToSystemDefault(reason)` → info-toast (8 с) с текстом из `crate::audio_output_message::audio_output_switch_message`; `RecoverableError` с kind `AudioDeviceUnavailable` → info-toast `AUDIO_OUTPUT_UNAVAILABLE_MESSAGE` вместо технического Display; прочие recoverable — как раньше (transient). Тесты: `state/notifications/audio_device_tests.rs`. Player-сторона: `mem:player-core/audio-runtime`.

## Спиннер ожидания данных (сессия 15, 2026-10-08)
- Решения владельца: только спиннер без текста; «ждём данные» = `PlaybackState::Buffering | Seeking` (в т.ч. seek на паузе), `Opening`/`Scrubbing` — нет; задержка появления 500 мс константой `BUFFERING_INDICATOR_APPEAR_DELAY`.
- Владелец: `state/notifications/buffering.rs` — `PlaybackWaitingWatch` (поле `NotificationCenter::playback_waiting`), вход `NotificationCenter::observe_playback_waiting(PlaybackState, now)` из `AppState::notifications_frame` до `frame()`. Эпизод ожидания непрерывен (Seeking→Buffering не сбрасывает таймер); выход прячет спиннер в том же кадре. Классификация состояний — исчерпывающий `match` (`waits_for_media_data`).
- `NotificationsFrame.buffering: BufferingIndicator::{Hidden, Visible}`; `frame()` сам прячет спиннер при занятом `center` (ошибка/прогресс открытия). Intent: `NotificationsFrame::shows_buffering_indicator()`.
- Будильник: `next_wake_deadline` включает момент появления, пока срок не наступил хотя бы в одном кадре (`revealed`) — после этого прошедший deadline не возвращается (иначе окно будилось бы бесконечно).
- Отрисовка: `ui/buffering_indicator.rs` (угол из `input.time` + `request_repaint` при Standard; фиксированный угол и без repaint при Reduced), фигура — `ArtworkPainter::buffering_spinner` (`ui-artwork-egui/src/buffering_spinner.rs`, подложка + дуга 3/4 круга; проверка `rect.is_finite() && is_positive()` — `f32::min` пропускает NaN).
- Тесты: `state/notifications/buffering/tests.rs` (фальшивое время), `state/center_overlay_tests.rs` (настоящий кадр egui: спиннер в центре, ошибка вместо спиннера, вращение/repaint только при Standard; repaint проверять на `app_behavior_context()` и после устоявшихся кадров), artwork — `buffering_spinner::tests`.

## Перерисовка (важно)
- В этом приложении `ctx.request_repaint_after(d)` делает `has_requested_repaint()` истинным, и окно перерисовывается **немедленно**: задержка игнорируется (`frame_prepare/ui_prepare.rs`). Для таймеров используйте `AppRenderFrameResult.next_ui_wake_deadline` (`earliest_ui_wake_deadline([..])` в `frame_prepare.rs`), куда добавлен `next_notification_wake_deadline()`. `ctx.request_repaint()` — только пока идёт анимация.

## Прочее
- Повторный Open во время открытия → toast «Файл ещё открывается» (`AppState::open_file`); `StrongMediaOpenUserOutcome::Silent` разделён на `Cancelled` (молча) и `Busy` (тот же toast в `report_prepared_local_install_failure`; в startup — прежний технический текст).
- «Сохранённая позиция … недоступна» (`startup_media/pending_install.rs`) теперь инфо-toast; отметка startup readiness `PreparationFailed` сохранена как была.
- Тексты ошибок не менялись: toast показывает `PlayerError` Display (`"SeekUnavailable: …"`) — бэклог.

## Тесты
- `state/notifications/tests.rs` — жизненный цикл на фальшивом времени + сквозные с настоящим `PlayerSession` (seek без seekable timeline → toast; `mark_fatal_error` → центр до ×; snapshot не меняется).
- `state/notifications/render_tests.rs` — настоящий egui-кадр, клики по × (наведение/нажатие/отпускание), позиция в углу, истечение, отсутствие repaint при reduced motion (тест поймал встроенный fade Area).
- `state/center_overlay_tests.rs` — приоритеты overlay и сквозной toast через `render_center_overlay`. Запуск: `cargo test -p app-egui --locked -- notifications center_overlay`.

Связанные: `mem:app-egui/center-overlay-state-2026-09-05`, `mem:app-egui/local-open-error-messages-ux02`, `mem:settings-ui/reduced-motion-2026-07-18`.
