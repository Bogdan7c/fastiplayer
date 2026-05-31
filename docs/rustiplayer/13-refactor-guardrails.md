# 13. Refactor Guardrails

Этот документ фиксирует проверяемые границы после серии refactoring PR.
Он описывает фактическую карту зависимостей, закрытые временные exceptions и
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
- `render-wgpu-shell` - WGPU instance/device/surface lifecycle, `winit`/`egui`
  composition, frame timing, submit/present и shell-facing render frame
  assembly.

## Source/network crates

Source/network crates владеют byte-access utilities и не должны становиться
service/player/render API.

- `media-prefetch` - config-agnostic RAM read-ahead wrapper поверх
  `source-core::ByteSource`.

## Concrete backend crates

Concrete backend crates владеют конкретной реализацией контейнера, аудио,
hardware decode или GPU render path. Они могут зависеть от contract crates, но
не должны становиться contract API для соседних слоёв.

Текущий список concrete backend crates:

- `symphonia-demux` - concrete adapter поверх upstream Symphonia для audio/container demux.
- `webm-demux` - compatibility re-export старого crate path на время transition.
- `audio` - concrete Symphonia/Opus decoder factory и CPAL output backend.
- `video-vaapi` - VA-API decoder thread, probe, DMA-BUF export и lifecycle
  decoded surfaces до renderer release.
- `render-wgpu-video` - NV12/P010 WGPU renderer, renderer-side DMA-BUF import,
  materialization API и shader paths.

`video-vulkan` удалён из workspace и Cargo graph. Его нельзя возвращать как
reference backend или hidden production dependency без отдельного
архитектурного решения.

Миграция Symphonia закрыла активный долг локального fork-а: workspace использует
upstream `symphonia = 0.6`, а устаревшие локальные каталоги патчей Symphonia
удалены из workspace и больше не участвуют ни в Cargo graph, ни в source tree.

## Current dependency map

Фактическая карта direct normal-dependencies, важная для архитектурных границ:

```text
app-egui -> player-core/service-youtube/desktop-integration
app-egui -> symphonia-demux/audio/video-core/video-vaapi/render-core/render-wgpu-shell/render-wgpu-video/source-core
app-egui -> media-core/capability-core/wgpu/winit/egui/egui-winit/rustiplayer-config
player-core -> media-core/codec-core/capability-core/video-core/video-backend-api/rustiplayer-config/audio-core/render-core
video-backend-api -> video-core
service-youtube -> source-core/media-prefetch/symphonia-demux/rustiplayer-config/capability-core/codec-core/media-core
source-core -> rustiplayer-config
media-prefetch -> source-core
symphonia-demux -> source-core/media-core/codec-core
webm-demux -> symphonia-demux
audio -> audio-core
codec-core -> vp9-parser
capability-core -> codec-core/render-core
render-core -> codec-core
video-core -> media-core/codec-core
video-vaapi -> video-backend-api/video-core/media-core/codec-core/capability-core
render-wgpu-video -> render-core/video-core/video-backend-api/codec-core/wgpu/ash/wgpu-types
render-wgpu-shell -> render-wgpu-video/render-core/wgpu/egui/egui-wgpu/winit
```

Карта намеренно отражает current production path. VA-API остаётся decoder
boundary, а WGPU/Vulkan import принадлежит renderer boundary; `video-vaapi`
не зависит от `wgpu`, `wgpu-types` или `ash`.

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
  render-wgpu split into render-wgpu-video and render-wgpu-shell
  render-wgpu-video owns video rendering without shell/reference backend deps
  render-wgpu-shell owns winit/egui surface composition
  video-vulkan removed from workspace and Cargo graph
```

## Временные нарушения

Temporary dependency debt для старого mixed `render-wgpu` crate отсутствует.
`render-wgpu-shell -> egui/egui-wgpu/winit` теперь является штатной shell
boundary, а не исключением. `render-wgpu-video` не зависит от shell/UI crates и
не зависит от удалённого `video-vulkan`.

Закрытые нарушения `player-core -> symphonia-demux/webm-demux`,
`player-core -> video-vaapi`, обратная dependency от `video-vaapi` к
`player-core`, `player-core -> wgpu` и `player-core -> audio` не должны
возвращаться. Local/YouTube opening остаётся
за shell/service layer и за `PreparedMedia`, production backend startup/resource
provider boundary остаётся в `video-backend-api`, WGPU materialization остаётся
в `app-egui`/`render-wgpu-video`, WGPU present path остаётся в
`render-wgpu-shell`, а production audio factories остаются в `audio` и
передаются через `audio-core` contracts.

## Dependency guardrails

Новые refactoring PR должны соблюдать эти правила:

- Contract crates не добавляют прямые зависимости на `app-egui`, `player-core`,
  `symphonia-demux`, `webm-demux`, `audio`, `video-vaapi`,
  `render-wgpu-shell`, `render-wgpu-video`, `video-vulkan`,
  `service-youtube`, `desktop-integration`, `wgpu`, `winit`, `egui`,
  `egui-winit`, `egui-wgpu`, `wgpu-types`, `ash`, `cros-codecs` или
  `cros-libva`.
- `media-core`, `codec-core`, `audio-core`, `audio`, `symphonia-demux` и `webm-demux` не
  добавляют прямые зависимости на `wgpu`, `video-vaapi`, `video-vulkan`,
  `render-wgpu-shell`, `render-wgpu-video`, `wgpu-types` или `ash`.
- `player-core` не добавляет новые direct dependencies на UI/shell/service,
  `symphonia-demux`, `webm-demux`, `video-vaapi`, `render-wgpu-shell`,
  `render-wgpu-video`, `video-vulkan`, `audio`, `wgpu`, `wgpu-types`, `ash`
  или другие concrete backend crates.
- `render-wgpu-shell` не начинает знать demux/source/audio/player/service или
  concrete video backend crates.
- `render-wgpu-video` не начинает знать shell/UI/app/player/service или
  concrete video backend crates.
- `media-prefetch` остаётся config-agnostic source wrapper: разрешены только
  direct dependencies на `source-core`, `tracing` и `thiserror`; запрещены
  `service-youtube`, `rustiplayer-config`, `player-core`, `app-egui`,
  render crates, containers, codecs и concrete backend crates.
- `video-vaapi` и будущие concrete video backend crates не зависят от
  `player-core`; backend startup/resource provider boundary проходит через
  `video-backend-api`.
- `video-vaapi` не зависит от renderer/GPU import crates (`wgpu`,
  `wgpu-types`, `ash`, `render-wgpu-video`, `render-wgpu-shell`): он владеет
  VA display, cros decoder, VA surfaces, DMA-BUF export и release lifecycle,
  но не создаёт WGPU texture views.
- `video-vulkan` не возвращается в workspace и не становится dependency
  production crates без отдельного архитектурного решения.
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
- запрещает молчаливое возвращение удалённых workspace crates, сейчас
  `video-vulkan`;
- запрещает прямые normal-dependencies из contract crates в shell/backend/player;
- запрещает прямые `media-core`/`codec-core`/`audio`/demux dependencies на
  `wgpu`, `wgpu-types`, `ash`, `video-vaapi`, `video-vulkan`,
  `render-wgpu-shell` и `render-wgpu-video`;
- запрещает возвращение `player-core -> symphonia-demux/webm-demux`,
  `player-core -> video-vaapi`, `player-core -> audio`, `player-core -> wgpu`,
  `player-core -> wgpu-types` и `player-core -> ash`;
- запрещает прямую dependency от `video-vaapi` к `player-core`, `wgpu`,
  `wgpu-types`, `ash`, `render-wgpu-video` и `render-wgpu-shell`;
- запрещает новые прямые связи `player-core`, `render-wgpu-shell` и
  `render-wgpu-video` с явно опасными соседними слоями;
- запрещает `media-prefetch` добавлять любые normal-dependencies кроме
  `source-core`, `tracing` и `thiserror`;
- не содержит temporary debt allowlist для old mixed `render-wgpu` crate.

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
- Следить, чтобы `render-wgpu-shell` оставался shell boundary, а
  `render-wgpu-video` оставался video-renderer boundary без UI/player/backend
  dependencies.
- Сравнивать новые public/internal boundary methods с tests на absent resource,
  active fake/stub, typed error и accounting no-op cases.
