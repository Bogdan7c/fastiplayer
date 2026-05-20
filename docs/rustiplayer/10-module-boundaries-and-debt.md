# 10. Границы модулей и долг

Этот файл фиксирует места, где код уже работает, но границы ещё не полностью
разделены. Это не баг-лист для немедленного переписывания, а карта рисков.

## Текущие временные exceptions

После последних refactor PR прямые зависимости `player-core -> symphonia-demux/webm-demux` и
`player-core -> video-vaapi` закрыты. Оставшийся Cargo/source-level долг:

- `player-core -> audio`: playback session всё ещё использует concrete Opus/CPAL
  audio path, а не neutral audio decoder/output factory.
- `player-core -> wgpu`: render lease и decoder boundary возвращают WGPU texture
  views для текущего zero-copy path.
- `video-vaapi -> player-core`: concrete backend crate реализует
  `VideoBackendFactory`, который пока объявлен в `player-core`.
- `render-wgpu -> egui/egui-wgpu/winit`: crate совмещает WGPU video renderer и
  shell composition.
- `render-wgpu -> video-vulkan`: reference/experimental Vulkan code всё ещё
  подключён к production renderer crate.

Эти exceptions не делают контрактные crates backend-specific: `media-core`,
`codec-core`, `video-core`, `render-core` и `capability-core` остаются
нейтральными contract boundaries.

## `player-core::PlayerSession`

`PlayerSession` всё ещё крупный объект. Часть обязанностей уже вынесена в
`pipeline`, `media_opening`, `seek_controller`, `seek_state`, `worker`, но session
по-прежнему содержит много orchestration logic:

- media open/install/reset;
- command dispatch;
- seek commit gates;
- audio/video scheduler helpers;
- capability refinement;
- render lease accounting;
- diagnostics aggregation.

Следующий безопасный шаг: выделять не новые абстракции ради размера файла, а
устойчивые поддомены с тестами: media opening, seek transaction, diagnostics sink,
video backend binding.

Media opening уже вынесен из прямой зависимости `player-core -> symphonia-demux/webm-demux`:
`player-core` принимает `PreparedMedia`. Оставшаяся работа здесь не в том, чтобы
вернуть concrete opener внутрь session, а в том, чтобы уменьшить orchestration
surface самого `PlayerSession`.

## `player-core::PlaybackPipeline`

`PlaybackPipeline` больше не является широким `pub(crate)` хранилищем runtime
slots. Struct остаётся crate-visible как внутренний владелец состояния
`player-core`, но его поля закрыты. `session.rs`, `tick.rs`, `worker.rs`,
`render_lease_bridge.rs` и snapshot builder должны обращаться к pipeline через
intent methods, описанные в
[09. Контракты и Internal API](09-contracts-and-internal-api.md), а не через
конкретное устройство storage.

Закрытые домены уже включают media source/demux, track selection, active video
requirement, seek generation, audio runtime/clock, packet queues, presentation
queues, video decoder I/O, in-flight packet accounting, render generation и
render lease accounting.

Оставшийся долг:

- surface area boundary methods всё ещё широкий, потому что `PlayerSession` и
  tick/scheduler пока разделяют orchestration внутри одного crate;
- `select_video_track_preserving_active_requirement()` является transitional
  legacy command path. TODO хранится рядом с методом: удалить его, когда выбор
  video track будет всегда передавать заново проверенный
  `VideoDecodeRequirement`;
- helper packet records `DecodedAudioPacket`, `PendingAudioPacket` и
  `PendingVideoPacket` всё ещё имеют `pub(crate)` поля. Это отдельный transport
  scope, не полевая граница `PlaybackPipeline`.

Следующий безопасный шаг: сужать не поля, а слишком крупные orchestration
домены: media opening, seek transaction, decoder I/O scheduler и diagnostics.
Каждое сужение должно идти с focused tests на absent resource, active fake/stub,
typed error/no-op и accounting edge cases.

## `app-egui::AppState::player_snapshot()`

Метод не является чистым getter-ом: он читает latest snapshot, обновляет cache и
публикует snapshot в desktop integration. Это практично для текущего shell, но
граница "read snapshot" и "publish desktop state" смешана.

Следующий шаг: разделить `refresh_player_snapshot()` и `publish_desktop_snapshot()`
или сделать явный per-frame shell context.

## `app-egui::main.rs`

`main.rs` всё ещё связывает winit lifecycle, renderer frame, YouTube startup job,
redraw pacing и error mapping. Это shell-level код, но файл остаётся плотным.

Следующий шаг: выделить shell runtime module без переноса player logic обратно в UI.

## WGPU texture bridge в `player-core`

`player-core` больше не содержит `WgpuVideoBackendFactory` и не зависит от
`video-vaapi`, но WGPU-specific boundary ещё не закрыт полностью.
`WgpuRenderTextureProviderHandle`, `WgpuRenderTextureViewLookup` и
`WgpuRenderTextureViews` находятся в `player-core`, потому что current production
lease flow материализует WGPU texture views из decoder-owned DMA-BUF resources.

Следующий шаг: выделить renderer-neutral resource materialization contract
между `player-core`, decoder backend-ом и render backend-ом. Это должно
сохранить различие между absent resource, busy texture pool, missing handle,
fatal backend error и нормальным release path.

## `video-vaapi` adapter boundary

`VaapiWgpuVideoBackendFactory` теперь живёт в `video-vaapi` и реализует
`player-core::VideoBackendFactory`. Это закрывает прежний долг
`player-core -> video-vaapi`, но создаёт обратную adapter-зависимость
`video-vaapi -> player-core`.

Следующий шаг: когда появится второй production decoder backend, перенести
startup/decoder-handle contract из `player-core` в более нейтральный crate
(`video-core` или отдельный backend API). До этого dependency допустима как
adapter debt, потому что ownership decoder thread-а остаётся у `video-vaapi`.

## `service-youtube`

`service-youtube` теперь умеет отдавать capability-aware stream candidates из
manifest metadata, но основной startup/playback path всё ещё использует старый
SDR-safe selector и возвращает уже открытый demuxer для совместимости. Поэтому
интеграция `capability-core` в реальный YouTube startup остаётся отдельным
архитектурным шагом.

Следующий шаг: service candidates -> capability selection -> demux open без
изменения HTTP refresh/range boundary.

## `symphonia-demux`

`symphonia-demux` владеет concrete adapter-ом поверх Symphonia и открывает
audio containers, которые поддерживает текущая Cargo feature set Symphonia. Старый
crate `webm-demux` оставлен только как compatibility re-export на transition PR,
чтобы внешние call sites могли мигрировать без одновременного изменения
поведения demux/seek/decode.

Миграция Symphonia закрыла активный долг локального fork-а: workspace dependency
теперь идёт в upstream `symphonia = 0.6`, а устаревшие локальные каталоги патчей
Symphonia удалены из workspace. Demux/audio path больше не имеет локального
Symphonia fork как source-level или Cargo-level fallback.

`infer_track_kind()` пока считает всё non-audio видео. Unknown video codec уже не
маскируется под VP9, но определение kind всё ещё грубое.

Следующий шаг: опираться на container metadata там, где Symphonia/Matroska
pre-scan даёт явный TrackType.

## `render-wgpu`

NV12 и P010 paths разделены, но WGPU shell, egui composition и video renderer
живут в одном crate. Это нормально сейчас, но при появлении второго renderer
backend-а нужно удержать `render-core` как общий contract, а не переносить туда
WGPU-specific детали.

## Patched dependencies

`cros-codecs` и `cros-libva` patched локально. Symphonia patch закрыт полностью:
активная dependency идёт через upstream `symphonia = 0.6`, локальный fork удалён,
и это больше не является source-level долгом между demux/audio и VA-API/WGPU.

Следующий шаг: периодически проверять upstream и удалять patches только после
прохождения zero-copy/HDR regression matrix.
