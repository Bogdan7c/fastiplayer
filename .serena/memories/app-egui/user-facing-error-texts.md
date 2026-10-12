# Пользовательские ошибки: английский текст

Актуальный языковой контракт (2026-10-12): `mem:app-egui/user-facing-language`. UI, уведомления, terminal diagnostics и служебные команды используют английский независимо от `ui.language`. Русские комментарии/документация и пользовательские Unicode-данные сохраняются.

`Display`/`Debug` PlayerError/SettingsError предназначены для диагностики. В интерфейс ошибка проходит через `app-egui/src/player_error_message.rs` (`player_error_user_message`, `player_error_row_summary`, `player_error_kind_phrase` — exhaustive match по PlayerErrorKind). Новый PlayerErrorKind намеренно ломает компиляцию до добавления понятной фразы. Пересборка после настроек: `settings_rebuild_message.rs`; причины web-открытия: `web_open_message.rs`. Сырые технические причины, Rust enum names и внутренние id не должны утекать в UI.

Статус настроек (`settings_ui/layout.rs::render_status`): `settings_runtime/status_text.rs` владеет summary/details Apply/Preview/Cancel/Reset; `setting_labels.rs::SettingLabels` читает английский label из registry, неизвестный id становится `unknown setting` плюс warn в лог. `settings_error_message.rs` exhaustive match-ом преобразует SettingsError/SettingValueError. `SettingsRuntime::report_runtime_error` принимает SettingsError, а не ToString. Технические route messages пишутся через tracing; язык английский сам по себе больше не признак технической ошибки. ApplyRouteResult::Failed{message} остаётся нетипизированным: пользователь получает обобщённую понятную причину; точная причина потребует отдельного архитектурного решения.

Apply с фоновой пересборкой показывает `Applying settings…`; повторное действие сообщает, что предыдущее применение продолжается. Cancel/suspend/exit сохраняют прежние typed final states и rollback semantics.

`media-source-open/src/web_open_failure.rs`: HlsProfileError -> NoPlayableFormat; HlsParseError -> InvalidServerResponse; нетипизированные runtime failures могут остаться Unclassified. Сырой текст доступен в диагностике, не в UI.

Статус отказа ручной навигации зависит от настоящего PlaybackState: `ui/playlist/status/previous_playback.rs::PreviousPlayback` (Playing/Paused/NothingPlaying), передаётся в PlaylistShowInput из sidebar. Очередь не владеет состоянием плеера и не определяет такую фразу.

Проверки: `settings_runtime/tests/status_text_user_facing.rs` запрещает id, Debug и raw owner reason, проверяет читаемый английский; `player_error_message.rs`, `web_open_message/tests.rs`, `state/notifications/*tests.rs` проверяют итоговые сообщения; `settings_runtime/tests/english_texts.rs` проверяет нарисованные настройки при ru/en и кликабельность Apply/Cancel в панели шириной 400 points. Unicode в именах/metadata пользователя не запрещать.