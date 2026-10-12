# Язык пользовательских текстов Fastiplayer

Решение владельца 2026-10-12: все собственные тексты Fastiplayer и инструментов проекта — американский английский: UI, accessibility names, системным диалогам переданные заголовки/фильтры, уведомления, ошибки, tracing/log, Display/thiserror/error context, stdout/stderr, build.rs, CLI help и сообщения проверок/assertions. Русские комментарии и документация сохраняются. Имена медиа/путей/устройств, пользовательские данные и Unicode-фикстуры не переводятся; собственные сообщения сторонних программ и стандартные элементы системных диалогов контролируются ими.

`ui.language` сохранён: default `ru`, прежние validation/storage/apply routes, без schema bump и миграции. Поле зарезервировано для будущей локализации; сейчас английский независимо от значения. Localization keys и имена label_ru/description_ru/help_ru/fallback_ru сохранены как совместимый контракт; их текущие пользовательские строки английские. Объяснение есть в config ui schema, settings-core SettingText и settings-derive parsing metadata.

Строка меняется в модуле-владельце. App error adapters по-прежнему скрывают raw technical reason/Debug/internal id. Backend/source/render ownership и typed error handling не менялись. Счётчики используют английское единственное число только для ровно 1 (21/111 — plural). Предпочтительные действия: Play, Pause, Stop, Playlist, Settings, Apply, Cancel, Reset, Retry.

Новый текстовый контракт логов:
- `Final seek commit completed`
- `Final seek commit stopped on timeout`
- `Demuxer reported a track list update`
- `Final seek commit continued with audio gate soft fallback` (распознаваемый analyzer marker)

Анализаторы `scripts/parse-seek-diagnostics.py`, `playback_acceptance.py` и их tests, smoke regex и benchmark collector используют английские сообщения. Не возвращать поддержку старых русских маркеров. Structured log fields, Rust API/setting/widget ids, JSON/CSV schemas не менялись.

Регрессии: `settings_runtime/tests/english_texts.rs` сравнивает реально нарисованные подписи/описания ru/en (само буквальное значение поля сохраняется), проверяет отсутствие кириллицы в собственных текстах и границы/клики Apply/Cancel при минимальной допустимой ширине сайдбара MIN_SIDEBAR_WIDTH_POINTS (350x300 points). Config metadata tests проверяют читаемые английские fallback. Существующие Playlist/button/confirmation/status/notification rendering tests проверяют финальный текст. Windows-1251 и Unicode media-title fixtures должны оставаться исходными, иначе исчезает смысл encoding/escaping тестов.

Подробный аудит оставшихся кириллических literals по файлам и строкам: `docs/user-facing-language.md`.

После изменения длины legacy files обновлять `scripts/module-size-baseline.json` только для существующих legacy entries по итоговому rustfmt. При поиске кириллицы отделять настоящие literals от Rust char literals и комментариев; `#[expect(..., reason = "...")]` — пояснения для разработчика, а не runtime output.