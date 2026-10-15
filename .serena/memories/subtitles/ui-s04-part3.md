# Субтитры: кнопка CC, меню, хоткеи, уведомления (сессия 04, часть 3, 2026-10-15)

## Владельцы и границы
- `subtitle-core::language_table` (приватная таблица ~42 языка) -> `SubtitleLanguage::canonical_primary_language()` (rus->ru) и `english_display_name()`. Автовыбор (`state/subtitles/auto_select.rs`) сравнивает канонические формы, предпочтения нормализуются так же.
- `state/subtitles/control_actions.rs`: `SubtitleOwner::apply_control_action(SubtitleControlAction) -> ControlActionFeedback::{Silent, Toast, ReplacingToast{key,text}}` — вся логика C/V/Z/X/меню (правило «нет дорожек» = toast "No subtitles for this video", предпочтение не трогается). `state/subtitle_control_actions.rs` — только glue AppState: `deliver_feedback` в `NotificationCenter`, диалог, `poll_subtitle_file_picker` (вызывается из `poll_subtitle_jobs`). Тонкие обёртки AppState toggle/select/nudge удалены (не нужны, тесты идут через владельца).
- `subtitle_message.rs` — чистые тексты: `track_text`, `sync_offset_text/toast/menu_text`, `unavailable_reason_text`, `attach_outcome_message`.
- `subtitle_file_open.rs` — `SubtitleFilePicker` (rfd AsyncFileDialog, owner mailbox, порт `AppWakeOwner::Subtitles`, защита от двух диалогов, Drop НЕ ждёт поток); `open_dialog_future` — вход для тестов.
- `NotificationCenter::notify_transient_replacing(TransientReplaceKey::{SubtitleSync, SubtitleTrack}, ..)` (`notifications/replacing.rs`).
- UI: `ui/player_controls/subtitle_control.rs` (+ `layout.rs` чистая раскладка/режимы, `button.rs`, `menu.rs`, `tests/`), `SubtitleControlAction`, `ControlAction::Subtitles`. Артворк: `ui-artwork-egui/src/subtitles_button.rs` (`ArtworkPainter::subtitles_button/subtitles_menu_arrow`). Токены в `ControlsStyle::subtitles_*`.
- Раскладка: `queue_mode_controls::control_layout_reserving_trailing_space(.., TrailingReservedWidth)`; CC прижата к Fullscreen, Shuffle/Repeat симметрично сжимаются на `progress*(footprint+12)`. Режим по ширине окна (пол 20 pt полей): Hidden < 470, Compact 470..498, Full >= 498; при >= ~594 ничего не двигается. Нет дорожек -> ни виджета, ни rect (раскладка совпадает с прежней).
- Хоткеи `ui/global_hotkeys.rs`: C, V, Z (Earlier), X (Later), без модификаторов; winit-путь их не перехватывает.
- Запоминание выбора: `settings_runtime/subtitle_enabled_persistence.rs` (debounce 3 с, flush на exit/suspend в `app_shell/runtime_settings_flush.rs`, покадрово в `frame_prepare/runtime_settings_persistence.rs`).

## Заглушки до части 4
`AppState::queue_subtitle_file_for_next_media`, `clear_queued_subtitle_files`, `QueueOutcome` помечены `expect(dead_code)` (бросок файла).

## Тесты
`subtitle_control/tests/{layout,widget}.rs`, `state/subtitles/tests/control_actions.rs`, `state/subtitle_control_actions/tests.rs`, `settings_runtime/tests/subtitle_enabled.rs`, `subtitle_message.rs`, `subtitle_file_open.rs`, `notifications/replacing.rs`, `ui/global_hotkeys/tests.rs`, `ui-artwork-egui/src/subtitles_button.rs`.
