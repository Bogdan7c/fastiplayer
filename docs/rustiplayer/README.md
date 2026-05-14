# rustiplayer architecture docs

Этот каталог описывает целевую архитектуру проекта `rustiplayer`.

Документы фиксируют не только ближайший рефакторинг MVP, но и направление для полноценного аппаратно-ускоренного плеера. Исторические планы по VA-API остаются в `docs/superpowers/`, а этот каталог считается новой навигационной точкой для дальнейшей разработки. Файлы отдельных фаз остаются историческими артефактами закрытия фаз: если они говорят о будущей работе, текущий статус нужно сверять с roadmap и более поздними фазами.

## Содержание

- [01. Vision and Scope](01-vision-and-scope.md) - продуктовая цель, ограничения и функциональный scope.
- [02. Target Architecture](02-target-architecture.md) - слои системы и поток данных.
- [03. Project Map](03-project-map.md) - целевая карта crate'ов и ответственность каждого модуля.
- [04. Codecs and Capabilities](04-codecs-capabilities.md) - аппаратные декодеры, профили, HDR, матрица возможностей.
- [05. Config and Runtime Data](05-config-and-storage.md) - TOML-настройки и правило runtime-only данных.
- [06. Rendering, UI and Platform](06-rendering-ui-platform.md) - wgpu/Vulkan, GLES fallback, egui, MPRIS, мультиплатформа.
- [07. Services and Network](07-services-network.md) - YouTube-клиент, будущие сервисы, cache, streaming.
- [08. Development Roadmap](08-development-roadmap.md) - поэтапный план разработки в порядке приоритета.
- [09. Phase 8.5 SDR Color Pipeline Prep](09-phase-8-5-sdr-color-pipeline-prep.md) - подготовка SDR color pipeline перед HDR.
- [10. Phase 9 Full VP9 Completion](09-phase-9-vp9-completion.md) - полное VP9 capability/metadata/decode-readiness направление перед HDR.
- [11. Phase 10 HDR-to-SDR Baseline](10-phase-10-hdr-to-sdr-baseline.md) - HDR-to-SDR baseline поверх готового VP9/P010 контракта.
- [12. Live Seek, Timeline and Desktop Controls Sessions](11-live-seek-timeline-sessions.md) - декомпозиция live seek/timeline/MPRIS на самостоятельные рабочие сессии.
- [13. Smooth Playback and Zero-Copy Sessions](12-smooth-playback-zero-copy-sessions.md) - декомпозиция работ по идеально плавному 4k60+ воспроизведению, hard zero-copy invariant и future codec readiness.

## Ключевые решения

| Область | Решение |
| --- | --- |
| Название проекта | `rustiplayer` |
| Основная платформа | Linux-first |
| Оконная система | Wayland primary, X11 fallback |
| Видео decode | Только аппаратное ускорение, software fallback для видео отсутствует |
| Video memory path | `zero_copy_video_only = true`: production path требует DMA-BUF export/import, CPU upload/readback не конфигурируется |
| Audio decode | Software decode допустим |
| Linux video backend | VA-API primary, с поддержкой i965 и iHD |
| Bitstream probing | Только через проверенные parser'ы/адаптеры, без новых ad-hoc bit parser'ов в `player-core` |
| Renderer primary | `wgpu`/Vulkan |
| Renderer legacy | Отдельный будущий OpenGL ES 2.0 crate для SDR 8-bit NV12 |
| Color pipeline | Phase 8.5 ввёл явный SDR color pipeline contract; Phase 10 добавил отдельный P010/HDR BT.2446-C path без смешивания с `nv12_to_rgba.wgsl` |
| Phase 9 | Полная typed VP9 модель: Profile 0 SDR production path, Profile 2 10-bit P010 readiness, точные rejects для 12-bit и 4:2:2/4:4:4; Phase 9-era запрет production HDR снят только для Phase 10 path при passing capability intersection |
| Phase 10 | HDR-to-SDR baseline поверх Phase 9: P010 zero-copy only, BT.2446 Method C, PQ+HLG, SDR BT.709 output; native HDR output остаётся future work |
| Swapchain transfer | SDR path сохраняет `PreserveCurrentUnorm`; Phase 10 HDR path использует `ExplicitShaderOetf` поверх `Unorm`; `SrgbRenderTarget` остаётся future mode |
| Color metadata | Используем layered metadata с origin/confidence: manifest/container/bitstream/decoder/fallback |
| Test assets | Маленькие VP9 headers/metadata/conflict fixtures коммитятся в repo; большие media samples остаются external/manual с documented expected logs |
| SDR adjustments | В contract закладываются brightness/contrast/saturation/exposure и RGB gain/offset с identity defaults |
| BT.2020 SDR | NV12 BT.2020 SDR показывается как diagnostic fallback в SDR BT.709, а P010 BT.2020 SDR rejected до явного wide-gamut SDR path |
| Windows | Second target, через DX12 |
| macOS | Later target |
| FFmpeg | Полностью вне проекта |
| Config | TOML через `serde` |
| Config schema | Current config schema version `2`; live seek, network cache/read-ahead и `ui.skin` defaults живут в config |
| Persistent data | Отсутствует: SQLite/`rusqlite` слой удалён; durable seek/cache metadata и runtime index слой отсутствуют |
| Timeline model | `media-core` владеет neutral `MediaTime`/`MediaDuration`/`TrackTimestamp`/`TimelineSnapshot`; первые concrete adapters - WebM/YouTube/VP9/VA-API/wgpu/MPRIS |
| Seek backend | Native demuxer path: `DemuxSeekRequest` несёт только target time и режим; WebM/MKV video seek использует decode-safe point before target через Symphonia/Matroska `SeekHead`/`Cues`, без app-level byte-offset hints |
| Playback ownership | Runtime `PlayerSession` и media pipeline живут в потоке `PlayerWorker`; `app-egui` отправляет команды, читает latest snapshot/events и не вызывает `PlayerSession::tick()` напрямую |
| Render frame lease | `PlayerWorker::try_acquire_present_frame()` отдаёт `PresentFrameLease`/`PlayerPresentFrame` с handle, metadata, generation и stale flag; `wgpu::TextureView` создаются на render thread через render-side provider, а release идёт через RAII drop/ack |
| Worker channels | `player-core` использует `crossbeam-channel`; high-rate `UpdateScrub` идёт через bounded latest channel с policy `Drain Latest` |
| Services | Модульные crate'ы, компилируются в один бинарь |
| YouTube | Временный `yt-dlp` adapter живёт в `service-youtube`; default selector остаётся SDR VP9/Opus WebM, а HDR/VP9.2 YouTube checks требуют explicit override до capability-aware service candidates |
| DRM | Дальняя архитектурная возможность, не текущий scope |

## Текущий runtime status

- Live seek/timeline Session 1 закрыла neutral timeline contracts и config schema v2.
- Live seek/timeline Session 2 закрыла playback worker boundary, `SeekController` skeleton,
  latest snapshot/event streams, deterministic shutdown и command priority для scrub.
- Live seek/timeline Session 3 закрыла render frame lease acceptance pass:
  `PresentFrameLease` сохраняет zero-copy handle/metadata/generation/stale state,
  worker больше не создаёт `wgpu` views, render errors идут typed command/event в worker.
- Live seek/timeline Sessions 4-8 закрыли real demux seek, precise seek transaction,
  minimal timeline UI и desktop/MPRIS integration через worker command/snapshot boundary.
- Runtime `BackgroundIndexer`, `background-index-scan` thread, index diagnostics
  и app-level byte-offset hints удалены. Seek снова проходит через native demuxer
  path, а точность commit-а обеспечивает `PlayerSession` через decoder reset,
  pre-roll/drop и commit gates.
- Live seek/timeline Session 10 закрыла нормальный preview seek: первый scrub target
  отправляется сразу на drag start, worker throttles live preview seek-и, а
  `PlayerSession` различает preview/final seek transaction поверх одного
  playback pipeline.
- После ремонта seek bootstrap video final seek тоже стартует с decode-safe точки
  не позже target: decoder reset получает keyframe, а точность пользовательской
  позиции остаётся за pre-roll/drop и commit gates в `PlayerSession`.
- Аудио-треск после worker/audio правок устранён через CPAL playback anchor smoothing и
  packet-boundary-safe resampler. На тяжёлых 4k60 asset-ах остаются late video drops;
  это отдельная будущая задача render/present cadence profiling, не блокер текущего этапа.
- План устранения late video drops, запрета CPU fallback и подготовки smooth playback
  для будущих codec-ов описан в
  [Smooth Playback and Zero-Copy Sessions](12-smooth-playback-zero-copy-sessions.md).
