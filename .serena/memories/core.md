# Fastiplayer — core (точка входа)

Короткая карта текущего состояния. Хронология этапов S00–S42/N01–N15/AUD-* перенесена в `mem:archive/core-history-2026-05-to-09` — читать только для истории решений, не как текущие ограничения.

## Проект
- Rust-медиаплеер, Linux-first, `v0.1.0-alpha.1` опубликован. Имя и контракт переименования: `mem:project-identity/fastiplayer-2026-09-05`.
- Стек и toolchain (Rust 1.99, MSRV 1.99, edition 2024): `mem:tech_stack`.
- Правила кода и комментариев: `mem:conventions`. Порядок завершения задачи и проверки: `mem:task_completion`; команды: `mem:suggested_commands`; песочница тестов: `mem:testing/sandbox_policy`.
- Как вести память: `mem:memory_maintenance` (core = граф ссылок, не журнал).

## Карта доменов (state owner → память)
- Playback core (session/worker/pipeline, seek, install/restore): `mem:player-core/core`; аудио-рантайм: `mem:player-core/audio-runtime`.
- Видеодекодирование: контракт потока `mem:video-core/decoder-stream-boundary`; FFmpeg software `mem:video-ffmpeg/software-design`; VA-API `mem:video-vaapi/core`.
- Рендер: `mem:render-video/core`, раскладка render-core `mem:render-core/module-layout-s22`; frame-server/scrub `mem:frame-server/core`.
- Аудио: `mem:audio/core`.
- Demux: `mem:demux-api/core`, `mem:symphonia-demux/core`, `mem:mpeg-ts-demux/core`, `mem:flv-demux/core`; кодеки `mem:codec-core/h264` (+ h265/av1).
- Открытие источников без UI (local/direct/web-media leaves, вынесено из app-egui): `mem:media-source-open/core`.
- Источники байтов: `mem:source-core/core`; сетевые протоколы (HLS/DASH/Smooth/HDS/HTTP/FTP/yt-dlp): `mem:media-services/core`; XML: `mem:xml/core`.
- Очередь/плейлист: `mem:playlist/core`, `mem:playlist/state`, `mem:playlist/discovery`.
- Приложение (composition root): разделение состояния `mem:app-egui/state-split`, event loop `mem:app-egui/app-shell-event-loop-decomposition-s42-2026-08-27`.
- Настройки: UI `mem:settings-ui/design`, схема/хранилище `mem:config/schema-store-decomposition-s23`.
- Тесты/CI: coverage-gate удалён 2026-10-03 (`mem:testing/coverage`), фикстуры `mem:testing/media-fixtures`, smoke `mem:testing/playback-smoke`, CI `mem:ci/github-actions`; патчи зависимостей `mem:dependency-patches/core`.

## Действующие инварианты качества (проверяются автоматически)
- Тесты: тест проверяет результат, а не вызов (правило `AGENTS.md`); coverage-gate отсутствует.
- Паники: `[workspace.lints.clippy]` unwrap_used/expect_used/panic = warn, CI с `-D warnings`. Детали и приёмы: `mem:panic-invariant-policy`.
- Мёртвый код: в production-коде нет `allow(dead_code)`-заглушек; код только для тестов помечается `#[cfg(test)]`, поля/варианты, читаемые только тестами, — `#[cfg(test)]` или `#[cfg_attr(not(test), expect(dead_code, reason))]`. Итог чистки 2026-10-02: `mem:code-health/project-health-cleanup-2026-10-02`.
- Размер модулей: hard limit 800 строк, legacy-снимок `scripts/module-size-baseline.json` (точный ratchet, обновлять при любом изменении legacy-файла). Границы crate-ов: `scripts/check-refactor-guardrails.py`.

## Открытые направления
- Вынос web-media orchestration (~15k строк) из app-egui в crate `media-source-open` — план и сессии-промпты жили в `user/web-media-extraction/` (каталог удалён владельцем после завершения работы). Session-01 (листья) и session-02 (общие доменные типы: compose PreparedMedia, stream model, catalog) сделаны 2026-10-03, session-03 (разрыв цикла opener-ы ↔ web_media_open: intent/component_variants/capability probe в crate) — тоже 2026-10-03, session-04 (HLS/DASH opener-ы + refresh в crate) — 2026-10-03, session-05 (остаток дерева `web_media_open` в crate; в app фасад + сквозные `content_probe_tests`) — 2026-10-03; session-06 (проектирование) — 2026-10-03, решение владельца: порты не нужны; session-07 (2026-10-03) — чистая подготовка native startup (`native_startup`), `Native*`-типы + fallback owner (`native_web_source`), `SafeMediaLabel` в crate, Job/glue остались в app; открыта ручная проверка протоколов. Coverage-gate целиком удалён 2026-10-03 (решение владельца; серия patch coverage отменена) — `mem:testing/coverage`; детали выноса — `mem:media-source-open/core`.
- Обновление egui 0.34 → 0.36 и wgpu 29 → 30 (winit остаётся 0.30): план из трёх сессий жил в `user/egui-wgpu-upgrade/` (подготовлен 2026-10-04; каталог удалён владельцем после завершения). Порядок такой: 01 — egui 0.35 на wgpu 29 с эталоном; 02 — egui 0.36 + wgpu 30 без изменения поведения, DMA-BUF `initial_state = UNINITIALIZED`; 03 — корректное начальное состояние DMA-BUF. Решение владельца: поведение UI сохраняется как сейчас (владелец нейтрализаций: `app-egui::ui::egui_behavior`). Сессия 01 (egui 0.35.0) закоммичена 2026-10-04 (e145738e). Сессия 02 (egui 0.36.2 + wgpu 30.0.1) закоммичена 2026-10-04 (1873de64). Сессия 03 (DMA-BUF `initialLayout = UNDEFINED` + согласованный `initial_state`, format list) закоммичена 2026-10-04 после приёмки владельцем (план обновления egui/wgpu завершён); детали — `mem:render-video/core`. Статус сессий: `user/egui-wgpu-upgrade/results/`.
- UX edge cases: глобальный аудит 2026-10-04 → план сессий в `user/ux-edge-cases/` (публично в репозитории; README — решения владельца, общие правила, бэклог; сессия 18 — Open без подвисания окна). Решения владельца: в ошибках имя файла без пути к папке; битый config → `.bak` + defaults + предупреждение; `playlist.error_behavior` по умолчанию `skip` с уведомлением. Сессия 01 (хоткеи после клика + Esc без закрытия приложения) закоммичена 2026-10-04 после приёмки владельцем; детали — `mem:app-egui/app-shell-event-loop-decomposition-s42-2026-08-27`. Сессия 02 (понятные ошибки открытия локального файла: имя файла + причина, бейдж строки плейлиста) закоммичена 2026-10-04 после приёмки; детали — `mem:app-egui/local-open-error-messages-ux02`. Сессия 03 (типизированная причина отказа player-а в `PlayerRejected/PlayerFailed`, убрано ложное «worker недоступен») закоммичена 2026-10-05 после приёмки (агент по поручению владельца); детали — `mem:app-egui/media-open-coordinator-s10c` (UX03). Сессия 04 (единый владелец уведомлений: центр для фатальной ошибки с ×, toast-ы 5/8 с, стопка до 3; recoverable-ошибки player-а временные; «Файл ещё открывается») закоммичена 2026-10-05 после приёмки владельцем; детали и intent-API для следующих сессий — `mem:app-egui/notifications`. Статус сессий: `user/ux-edge-cases/results/`.
- Git: работать в `main`, отдельные ветки не создавать без указания владельца.
- `user/` — публичные markdown-планы сессий и отчёты `results/` (решение владельца 2026-10-05: workflow открыт сообществу, ignore `/user/` снят). Туда кладём только `.md`: никаких бинарников, логов, медиа, секретов и личных путей вне проекта. Прежнее правило S00 «user/ вне репозитория» (`mem:public-launch/s00-user-backup-2026-09-04`) отменено.
- Transport-команды во время playlist install (guard/terminal slot, исполнитель Runtime+AppState): `mem:app-egui/transport-guard-execution-2026-10-03`.
