# Документация rustiplayer

Актуализировано: 2026-05-29.

Этот каталог описывает текущее состояние `rustiplayer`, а не историю фаз
рефакторинга. Исторические session-планы удалены из рабочей навигации: если
нужно понять проект, начинаем отсюда и сверяемся с кодом.

## Навигация

- [01. Цель и Scope](01-vision-and-scope.md) - цель продукта и текущий scope.
- [02. Целевая архитектура](02-target-architecture.md) - runtime слои и основные потоки.
- [03. Карта проекта](03-project-map.md) - workspace crates и ответственность.
- [04. Кодеки и Capabilities](04-codecs-capabilities.md) - decode/render matrix и typed rejects.
- [05. Config и Runtime Data](05-config-and-storage.md) - TOML schema, defaults, runtime-only data.
- [06. Rendering, UI и Platform](06-rendering-ui-platform.md) - WGPU, render bridge, UI, MPRIS.
- [07. Services и Network](07-services-network.md) - `source-core`, YouTube adapter, HTTP Range.
- [08. Roadmap разработки](08-development-roadmap.md) - ближайшие направления после текущего refactor.
- [09. Контракты и Internal API](09-contracts-and-internal-api.md) - стабильные внутренние контракты.
- [10. Границы модулей и долг](10-module-boundaries-and-debt.md) - места, где границы ещё не чистые.
- [11. Аудит документации](11-documentation-audit.md) - что было неточным, устаревшим или логически неверным.
- [12. План усиления seek/scrub](12-seek-reliability-plan.md) - сессионный план фикса reliability проблем перемотки.
- [13. Refactor Guardrails](13-refactor-guardrails.md) - проверяемые границы crates перед refactoring PR.
- [14. Manual Seek/Scrub Acceptance](14-manual-seek-acceptance.md) - ручная media matrix и parser для seek diagnostics.

## Короткая карта

`app-egui` является shell: окно, egui, renderer wiring, команды в worker и
read-only snapshot. Media pipeline живёт в `player-core` внутри `PlayerWorker`.

Видео в production идёт только через hardware decode и DMA-BUF zero-copy.
Software video fallback, CPU upload и CPU readback не являются настройками.
Текущий video decode backend path - VA-API; FFmpeg software decode ещё не
добавлен, а Vulkan video decode path в проекте не поддерживается.
Vulkan-упоминания в render docs относятся к WGPU/Vulkan surface/import path, а
не к `video.preferred_backend`.

Текущий production video path:

- VP9 Profile 0, 8-bit, 4:2:0, SDR -> VA-API -> NV12 -> WGPU SDR path.
- VP9 Profile 2, 10-bit, 4:2:0, HDR PQ/HLG -> VA-API -> P010 -> WGPU BT.2446-C HDR-to-SDR path.

Текущий service path:

- Локальные media files открываются через `symphonia-demux`; `webm-demux`
  остаётся только compatibility re-export-ом старого crate path.
- YouTube пока проходит через `service-youtube` и `yt-dlp`.
- `source-core` владеет local/HTTP byte source, Range seekability и RAM byte-range cache.

## Инварианты

- `player-core` не должен импортировать codec-specific parser crate напрямую.
- `app-egui` не должен читать `PlayerSession` или `PlaybackPipeline`.
- Renderer получает frame только через `PresentFrameLease`.
- Config хранит пользовательские настройки, но не историю, cookies, bookmarks и durable cache metadata.
- Capability selection обязана пройти decode backend, memory contract, renderer import и color pipeline.
- Неуверенный bitstream probe не должен превращаться в fatal hardware failure.

## Seek Diagnostics

Минимальный локальный trace для baseline расследования seek/scrub:

```bash
RUST_LOG=player_core=debug,symphonia_demux=debug,app_egui=debug cargo run -p app-egui
```

В логах нужно смотреть цепочку `Starting demux seek transaction` ->
`Demux seek transaction accepted` -> первые `Post-seek ...` markers ->
`Active seek transaction is still waiting`, если seek не закрывается.
