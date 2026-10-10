# B6: предупреждение «Hardware без VA-API» в окне настроек (2026-10-10)

- Владелец: `app-egui/src/settings_runtime/hardware_decode_hint.rs`. `HardwareDecodeAvailability::{Unknown, Available, Unavailable}` выводится из `SystemCapabilities` (`from_system_capabilities`): Unavailable = vaapi backend не Available ИЛИ нет playable output с `DecodeBackendId::vaapi()`; vaapi нет в отчёте = Unknown.
- Источник истины — capability probe при старте (`app_shell/mod.rs` после `probe_system_capabilities`), нового probe нет. Intent: `SettingsRuntime::set_hardware_decode_availability` (инвалидирует `ui_model_cache` только при изменении). Пока не вызван — Unknown, строки нет.
- `SettingsRuntime::ui_fields` кладёт `SettingsUiField.status` для `video.preferred_backend`, когда ЧЕРНОВИК = Hardware и Unavailable (рисует `render_field_messages`, UI не менялся).
- Известное ограничение: availability не обновляется после `renderer_recreation` (VA-API на лету не меняется); поддержку кодеков не разбираем.
- Тексты ошибок: `local_open_message::player_install_failure_phrase(NoSuitableVideoDecoder)` и `player_error_message` (`RequiredVideoBackendUnavailable`) советуют режим «Авто».
- Тесты: `settings_runtime/tests/hardware_decode_hint.rs`.
