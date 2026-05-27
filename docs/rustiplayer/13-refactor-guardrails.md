# 13. Refactor Guardrails

Этот документ фиксирует проверяемые границы после серии refactoring PR.
Он описывает фактическую карту зависимостей, оставшиеся временные exceptions и
правило, что каждый следующий PR обязан сохранять behavior parity.

## Главный инвариант

Дальнейшие refactoring PR должны сохранять behavior parity: пользовательское
поведение, playback/render/seek/scrub semantics, HDR/P010/NV12 результат,
zero-copy path, лимиты очередей, error policy, diagnostics и config defaults
не меняются без отдельного архитектурного решения.

Если рефакторинг обнаруживает риск изменения поведения, PR должен остановиться
на boundary/TODO или вынести поведенческое изменение в отдельное обсуждение.

## Contract crates

Contract crates задают нейтральные типы и правила между слоями. Они не должны
зависеть от UI shell, конкретных demux/audio/video/render backend-ов или
`player-core` orchestration.

Текущий список contract crates:

- `media-core` - neutral media packets, tracks, timeline/time contracts.
- `codec-core` - codec/profile/color/surface/memory requirements.
- `video-core` - decoded frame, texture handle и video diagnostics contracts.
- `video-backend-api` - video backend startup/resource-provider boundary.
- `render-core` - renderer-neutral capabilities, color и render diagnostics.
- `capability-core` - selection gate между stream requirements и render/backend reports.

Разрешённое направление внутри contract слоя остаётся узким:

```text
media-core -> codec-core
video-core -> media-core / codec-core
video-backend-api -> video-core
render-core -> codec-core
capability-core -> codec-core / render-core
```

`codec-core -> vp9-parser` сейчас является внутренней codec-model деталью, а не
разрешением для `player-core` или UI импортировать codec-specific parser crates.

## App/Shell crates

Shell crates владеют процессом, окном, UI, lifecycle wiring и связыванием
production backend-ов. Shell может знать concrete crates, но не должен переносить
playback state или scheduling обратно из `player-core`.

Текущий список app/shell crates:

- `app-egui` - desktop process, winit lifecycle, egui UI, renderer wiring,
  startup jobs, desktop integration wiring, local media opening и production
  backend composition.
- `render-wgpu` shell часть - WGPU surface/swapchain setup, `winit`/`egui`
  composition и shell-facing render frame assembly.

## Concrete backend crates

Concrete backend crates владеют конкретной реализацией контейнера, аудио,
hardware decode или GPU render path. Они могут зависеть от contract crates, но
не должны становиться contract API для соседних слоёв.

Текущий список concrete backend crates:

- `symphonia-demux` - concrete adapter поверх upstream Symphonia для audio/container demux.
- `webm-demux` - compatibility re-export старого crate path на время transition.
- `audio` - concrete Symphonia/Opus decoder factory и CPAL output backend.
- `video-vaapi` - VA-API decoder thread, probe, DMA-BUF export/import.
- `render-wgpu` video backend часть - NV12/P010 WGPU renderer и shader paths.

`video-vulkan` остаётся experimental/reference crate в workspace и не является
production decode path для `PlayerWorker`.

Миграция Symphonia закрыла активный долг локального fork-а: workspace использует
upstream `symphonia = 0.6`, а устаревшие локальные каталоги патчей Symphonia
удалены из workspace и больше не участвуют ни в Cargo graph, ни в source tree.

## Current dependency map

Фактическая карта direct normal-dependencies, важная для архитектурных границ:

```text
app-egui -> player-core/service-youtube/desktop-integration
app-egui -> symphonia-demux/audio/video-vaapi/render-wgpu/source-core
app-egui -> wgpu/winit/egui/egui-winit/rustiplayer-config
player-core -> media-core/codec-core/capability-core/video-core/video-backend-api/rustiplayer-config/audio-core
video-backend-api -> video-core
service-youtube -> source-core/symphonia-demux/rustiplayer-config/capability-core/codec-core
source-core -> rustiplayer-config
symphonia-demux -> source-core/media-core/codec-core
webm-demux -> symphonia-demux
audio -> audio-core
video-vaapi -> video-backend-api/video-core/media-core/codec-core/capability-core/wgpu
render-wgpu -> render-core/video-core/codec-core/video-vulkan/wgpu/egui/egui-wgpu/winit
```

Карта намеренно отражает current production path, а не идеальную нейтральность:
VA-API и WGPU остаются частью production boundary.

## Before/after refactor map

```text
Before:
  player-core -> webm-demux
  player-core -> video-vaapi
  video-vaapi depended on player-core
  player-core -> audio
  player-core -> wgpu
  render-wgpu -> egui/winit/video-vulkan

After:
  player-core -> symphonia-demux/webm-demux closed
  player-core -> video-vaapi closed
  reverse video-vaapi/player-core edge closed
  player-core -> audio closed; player-core -> audio-core remains
  player-core -> wgpu closed
  player-core -> video-backend-api owns playback-facing backend boundary
  app-egui -> symphonia-demux/video-vaapi owns production composition
  app-egui -> audio wires production audio factories
  video-vaapi -> video-backend-api implements backend startup/resource provider
  render-wgpu -> egui/egui-wgpu/winit/video-vulkan remains
```

## Временные нарушения

Эти связи описывают текущий долг. Они допустимы только как compatibility debt и
не являются целевой архитектурой.

| Связь | Почему сейчас существует | Целевое направление |
| --- | --- | --- |
| `render-wgpu -> egui/winit/video-vulkan` | Crate одновременно содержит shell composition, WGPU renderer и reference Vulkan linkage. | Разделить shell/winit/egui wiring и production WGPU video backend; убрать reference dependency из production renderer path. |

`render-wgpu -> egui-wgpu` считается частью той же shell-composition проблемы,
хотя краткая debt-метка выше записана как `egui/winit`.

Закрытые нарушения `player-core -> symphonia-demux/webm-demux`,
`player-core -> video-vaapi`, обратная dependency от `video-vaapi` к
`player-core`, `player-core -> wgpu` и `player-core -> audio` не должны
возвращаться. Local/YouTube opening остаётся
за shell/service layer и за `PreparedMedia`, production backend startup/resource
provider boundary остаётся в `video-backend-api`, WGPU materialization остаётся
в `app-egui`/`render-wgpu`, а production audio factories остаются в `audio` и
передаются через `audio-core` contracts.

## Dependency guardrails

Новые refactoring PR должны соблюдать эти правила:

- Contract crates не добавляют прямые зависимости на `app-egui`, `player-core`,
  `symphonia-demux`, `webm-demux`, `audio`, `video-vaapi`, `render-wgpu`,
  `video-vulkan`, `service-youtube`, `desktop-integration`, `wgpu`, `winit`,
  `egui`, `egui-winit` или `egui-wgpu`.
- `media-core`, `codec-core`, `audio-core`, `audio`, `symphonia-demux` и `webm-demux` не
  добавляют прямые зависимости на `wgpu`, `video-vaapi` или `render-wgpu`.
- `player-core` не добавляет новые direct dependencies на UI/shell/service,
  `symphonia-demux`, `webm-demux`, `video-vaapi`, `render-wgpu`,
  `video-vulkan`, `audio`, `wgpu` или другие concrete backend crates.
- `render-wgpu` не начинает знать demux/source/audio/player/session crates.
- `video-vaapi` и будущие concrete video backend crates не зависят от
  `player-core`; backend startup/resource provider boundary проходит через
  `video-backend-api`.
- Новые обращения к `PlaybackPipeline` внутри `player-core` проходят через
  intent methods. Возвращать `pub(crate)` поля в сам `PlaybackPipeline`
  запрещено без отдельного архитектурного решения и focused tests.
- Новое исключение в dependency graph сначала документируется здесь с причиной,
  владельцем состояния, планом удаления и focused проверкой.
- Удаление временного нарушения разрешено без сохранения compatibility debt,
  если PR доказывает behavior parity тестами или ручной verification matrix.

## Проверка

Лёгкая проверка находится в `scripts/check-refactor-guardrails.py`.

Скрипт использует `cargo metadata --no-deps --format-version 1`, потому что Cargo
документирует этот JSON как источник workspace packages и manifest dependencies,
а `--format-version` фиксирует ожидаемый формат.

Текущая проверка намеренно маленькая:

- проверяет наличие зафиксированных role crates в workspace;
- запрещает прямые normal-dependencies из contract crates в shell/backend/player;
- запрещает прямые `media-core`/`codec-core`/`audio`/demux dependencies на
  `wgpu`, `video-vaapi` и `render-wgpu`;
- запрещает возвращение `player-core -> symphonia-demux/webm-demux`,
  `player-core -> video-vaapi`, `player-core -> audio` и `player-core -> wgpu`;
- запрещает прямую dependency от `video-vaapi` к `player-core`;
- запрещает новые прямые связи `player-core` и `render-wgpu` с явно опасными
  соседними слоями, кроме текущего temporary debt allowlist;
- печатает найденные временные нарушения как долг, но не считает их ошибкой.

Локальный pre-PR путь находится в `scripts/pre-pr-checks.sh`. Он последовательно
запускает:

- `cargo metadata --no-deps --format-version 1`;
- `scripts/check-refactor-guardrails.py`;
- `cargo fmt --all --check`;
- `cargo check --workspace`;
- `cargo clippy --workspace --all-targets`.

## TODO для будущих dependency checks

- Добавить transitive graph проверку через `cargo metadata` без `--no-deps`,
  когда появится стабильная policy для dev/build dependencies.
- Проверять split `render-wgpu` shell и video backend частей, включая
  `egui`, `egui-wgpu`, `winit` и `video-vulkan`.
- Сравнивать новые public/internal boundary methods с tests на absent resource,
  active fake/stub, typed error и accounting no-op cases.
