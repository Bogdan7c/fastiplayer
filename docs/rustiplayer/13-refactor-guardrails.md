# 13. Refactor Guardrails

Этот документ фиксирует проверяемые границы перед серией refactoring PR.
Он описывает целевую карту зависимостей, текущие временные нарушения и правило,
что каждый последующий PR обязан сохранять behavior parity.

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
- `render-core` - renderer-neutral capabilities, color и render diagnostics.
- `capability-core` - selection gate между stream requirements и render/backend reports.

Разрешённое направление внутри contract слоя остаётся узким:

```text
media-core -> codec-core
video-core -> media-core / codec-core
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
  startup jobs, desktop integration wiring.
- `render-wgpu` shell часть - WGPU surface/swapchain setup, `winit`/`egui`
  composition и shell-facing render frame assembly.

## Concrete backend crates

Concrete backend crates владеют конкретной реализацией контейнера, аудио,
hardware decode или GPU render path. Они могут зависеть от contract crates, но
не должны становиться contract API для соседних слоёв.

Текущий список concrete backend crates:

- `webm-demux` - Symphonia WebM/Matroska demuxer.
- `audio` - текущий Opus decoder и CPAL output path.
- `video-vaapi` - VA-API decoder thread, probe, DMA-BUF export/import.
- `render-wgpu` video backend часть - NV12/P010 WGPU renderer и shader paths.

`video-vulkan` остаётся experimental/reference crate в workspace и не является
production decode path для `PlayerWorker`.

## Временные нарушения

Эти связи описывают текущий долг. Они допустимы только как compatibility debt и
не являются целевой архитектурой.

| Связь | Почему сейчас существует | Целевое направление |
| --- | --- | --- |
| `player-core -> webm-demux` | `player-core` пока открывает локальный WebM/Matroska через concrete demuxer. | Перенести neutral `Demuxer` contract в `media-core`, затем открывать prepared media за пределами `player-core`. |
| `player-core -> audio::OpusDecoder` | Audio pipeline пока хранит concrete Opus decoder вместо codec-neutral boundary. | Ввести `audio::AudioDecoder`/factory и оставить Opus только concrete implementation. |
| `player-core -> video-vaapi` | Production backend startup пока создаётся из player factory. | Оставить в `player-core` только neutral video backend factory/handle contract. |
| `player-core -> wgpu` | Zero-copy interop сейчас протаскивает WGPU handles через startup boundary. | Выделить GPU interop boundary, чтобы player не зависел от конкретного graphics API. |
| `render-wgpu -> egui/winit/video-vulkan` | Crate одновременно содержит shell composition, WGPU renderer и reference Vulkan linkage. | Разделить shell/winit/egui wiring и production WGPU video backend; убрать reference dependency из production renderer path. |

`render-wgpu -> egui-wgpu` считается частью той же shell-composition проблемы,
хотя краткая debt-метка выше записана как `egui/winit`.

## Dependency guardrails

Новые refactoring PR должны соблюдать эти правила:

- Contract crates не добавляют прямые зависимости на `app-egui`, `player-core`,
  `webm-demux`, `audio`, `video-vaapi`, `render-wgpu`, `video-vulkan`,
  `service-youtube`, `desktop-integration`, `wgpu`, `winit`, `egui`,
  `egui-winit` или `egui-wgpu`.
- `player-core` не добавляет новые direct dependencies на UI/shell/service или
  дополнительные concrete backend crates сверх временно описанных нарушений.
- `render-wgpu` не начинает знать demux/source/audio/player/session crates.
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
- запрещает новые прямые связи `player-core` и `render-wgpu` с явно опасными
  соседними слоями, кроме текущего temporary debt allowlist;
- печатает найденные временные нарушения как долг, но не считает их ошибкой.

## TODO для будущих dependency checks

- Подключить `scripts/check-refactor-guardrails.py` в локальный pre-PR/CI путь.
- Добавить transitive graph проверку через `cargo metadata` без `--no-deps`,
  когда появится стабильная policy для dev/build dependencies.
- Проверять source-level debt `player-core -> audio::OpusDecoder`, потому что
  manifest видит только `player-core -> audio`.
- Проверять удаление `player-core -> webm-demux` после prepared-media boundary.
- Проверять удаление `player-core -> video-vaapi` и `player-core -> wgpu` после
  neutral video backend/GPU interop boundary.
- Проверять split `render-wgpu` shell и video backend частей, включая
  `egui`, `egui-wgpu`, `winit` и `video-vulkan`.
- Сравнивать новые public/internal boundary methods с tests на absent resource,
  active fake/stub, typed error и accounting no-op cases.
