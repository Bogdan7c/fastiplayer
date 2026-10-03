# Fastiplayer — core (точка входа)

Короткая карта текущего состояния. Хронология этапов S00–S42/N01–N15/AUD-* перенесена в `mem:archive/core-history-2026-05-to-09` — читать только для истории решений, не как текущие ограничения.

## Проект
- Rust-медиаплеер, Linux-first, `v0.1.0-alpha.1` опубликован. Имя и контракт переименования: `mem:project-identity/fastiplayer-2026-09-05`.
- Стек и toolchain (Rust 1.96, MSRV 1.92, edition 2024): `mem:tech_stack`.
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
- Тесты/CI: покрытие `mem:testing/coverage`, фикстуры `mem:testing/media-fixtures`, smoke `mem:testing/playback-smoke`, CI `mem:ci/github-actions`; патчи зависимостей `mem:dependency-patches/core`.

## Действующие инварианты качества (проверяются автоматически)
- Паники: `[workspace.lints.clippy]` unwrap_used/expect_used/panic = warn, CI с `-D warnings`. Детали и приёмы: `mem:panic-invariant-policy`.
- Мёртвый код: в production-коде нет `allow(dead_code)`-заглушек; код только для тестов помечается `#[cfg(test)]`, поля/варианты, читаемые только тестами, — `#[cfg(test)]` или `#[cfg_attr(not(test), expect(dead_code, reason))]`. Итог чистки 2026-10-02: `mem:code-health/project-health-cleanup-2026-10-02`.
- Размер модулей: hard limit 800 строк, legacy-снимок `scripts/module-size-baseline.json` (точный ratchet, обновлять при любом изменении legacy-файла). Границы crate-ов: `scripts/check-refactor-guardrails.py`.

## Открытые направления
- Вынос web-media orchestration (~15k строк) из app-egui в crate `media-source-open` — план и сессии-промпты в `user/web-media-extraction/` (приватно). Session-01 (листья) и session-02 (общие доменные типы: compose PreparedMedia, stream model, catalog) сделаны 2026-10-03, session-03 (разрыв цикла opener-ы ↔ web_media_open: intent/component_variants/capability probe в crate) — тоже 2026-10-03; следующая — session-04 (HLS/DASH opener-ы + refresh); coverage baseline переснимается один раз после сессии 5 (до этого 2 теста `test_coverage_metrics` красные ожидаемо) — `mem:media-source-open/core`.
- Git: работать в `main`, отдельные ветки не создавать без указания владельца.
- Transport-команды во время playlist install (guard/terminal slot, исполнитель Runtime+AppState): `mem:app-egui/transport-guard-execution-2026-10-03`.
