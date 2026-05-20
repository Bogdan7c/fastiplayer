# 10. Границы модулей и долг

Этот файл фиксирует места, где код уже работает, но границы ещё не полностью
разделены. Это не баг-лист для немедленного переписывания, а карта рисков.

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

## `player-core::WgpuVideoBackendFactory`

Название factory связано с WGPU handles, но фактически запускает текущий VA-API
decode backend. Это отражает zero-copy import reality, но имя может запутывать.

Следующий шаг: переименовать boundary вокруг "GPU interop video backend factory"
или добавить отдельный VA-API factory, не ломая public call sites.

## `service-youtube`

`service-youtube` стал модульнее, но public API всё ещё возвращает уже выбранный
demuxer, а не набор capability-aware stream candidates. Поэтому default selector
остаётся SDR-safe, а HDR YouTube проверки требуют explicit override.

Следующий шаг: service candidates -> capability selection -> demux open.

## `webm-demux`

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

`cros-codecs`, `cros-libva` и Symphonia crates patched локально. Это технический
долг поддержки совместимости.

Следующий шаг: периодически проверять upstream и удалять patches только после
прохождения zero-copy/HDR regression matrix.
