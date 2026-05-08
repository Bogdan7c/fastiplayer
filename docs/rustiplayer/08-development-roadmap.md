# 08. Development Roadmap

## Общая стратегия

Не переписывать MVP за один раз.

Движение должно быть инкрементальным:

1. Зафиксировать архитектуру.
2. Создать core crate'ы с минимальными типами.
3. Переносить логику из `app-egui` маленькими шагами.
4. После каждого шага сохранять воспроизводимость текущего VP9 MVP.
5. Только потом расширять codec/container/service matrix.

## Phase 0: Architecture docs

Статус: текущий этап.

Цель:

- зафиксировать product scope;
- зафиксировать crate map;
- зафиксировать capability model;
- зафиксировать roadmap.

Acceptance:

- документы лежат в `docs/rustiplayer/`;
- команда использует их как точку навигации.

## Phase 1: Extract `player-core` skeleton

Статус: реализовано.

Цель:

- создать `crates/player-core`;
- описать `PlayerCommand`, `PlayerEvent`, `PlayerSnapshot`, `PlaybackState`;
- не переносить сразу всю логику;
- подключить crate к workspace.

Acceptance:

- `cargo check` проходит;
- `app-egui` может импортировать типы;
- текущий playback проверен вручную и не сломан.

## Phase 2: Extract `media-core`

Статус: реализовано.

Цель:

- создать `crates/media-core`;
- перенести общие `Packet`, `TrackInfo`, `TrackKind`, `TimeBase`;
- обновить `webm-demux`, `audio`, `video-core`, `app-egui`.

Acceptance:

- media-типы больше не принадлежат `webm-demux`;
- WebM MVP работает;
- тесты demux/packet timestamp проходят.

## Phase 3: Move playback state out of `AppState`

Статус: реализовано.

Цель:

- перенести из `AppState` player поля в `PlayerSession`;
- оставить в `AppState` только egui-local state;
- UI получает `PlayerSnapshot`.

Переносимые поля:

- player state;
- draining EOF flag;
- current position/duration;
- demuxer;
- audio decoder/output/clock;
- video decoder thread;
- pending packet queues;
- video frame queue;
- present frame;
- frame duration estimate;
- playback errors.

Acceptance:

- `AppState` больше не владеет media pipeline;
- play/pause/open file работают;
- UI рисует snapshot.

## Phase 4: Move tick/scheduler out of `main.rs`

Статус: реализовано.

Цель:

- перенести demux loop;
- перенести audio packet processing;
- перенести video packet send/drain;
- перенести A/V scheduler;
- перенести backpressure config.

Новый API:

```rust
impl PlayerSession {
    pub fn tick(&mut self, tick_context: PlayerTickContext) -> PlayerTickResult;
}
```

Acceptance:

- `app-egui::render_frame` больше не читает demuxer;
- `main.rs` становится lifecycle/render shell;
- scheduler unit tests появляются в `player-core`.

## Phase 5: Config crate

Статус: реализовано.

Цель:

- создать `crates/config`;
- ввести TOML schema version;
- defaults;
- validation;
- user path `~/.config/rustiplayer/config.toml`.

Acceptance:

- приложение запускается без config и создает defaults;
- invalid config дает понятную ошибку;
- playback limits больше не hardcoded в app layer.

## Phase 6: Storage crate

Статус: реализовано.

Цель:

- создать `crates/storage`;
- подключить `rusqlite`;
- создать migration framework;
- создать базовые таблицы history/progress/capability cache.

Acceptance:

- SQLite создается в `~/.local/share/rustiplayer/rustiplayer.sqlite`;
- migrations transaction-safe;
- storage errors видны в UI/log.

## Phase 7: Capability core and VA-API probing

Статус: реализовано.

Цель:

- создать `codec-core`;
- создать `capability-core`;
- вынести VA-API probing;
- построить supported decode matrix.

Acceptance:

- UI может показать capability report;
- stream selection использует capabilities;
- unsupported profile дает понятную ошибку до decode.

## Phase 8: Renderer split

Цель:

- создать `render-core`;
- создать/переименовать `render-wgpu`;
- вынести `app-egui/src/render.rs`;
- описать `RenderableFrame` и `RenderCapabilities`.

Acceptance:

- app shell не знает детали NV12 renderer;
- renderer capabilities участвуют в stream selection;
- текущий NV12 VP9 path работает.

## Phase 9: HDR-to-SDR baseline

Цель:

- расширить frame metadata;
- добавить color metadata path;
- добавить HDR-to-SDR shader path;
- начать с PQ/HLG to SDR BT.709.

Acceptance:

- HDR input не отображается как washed-out SDR;
- UI показывает active color path;
- SDR видео не ломается.

## Phase 10: Full VP9 completion

Цель:

- закрыть текущие ограничения VP9;
- добавить profile/bit-depth handling;
- закрепить VP9 header probing как adapter с мягким поведением при ошибке;
- добавить sanity-check bitstream metadata против container/service metadata, если она доступна;
- обновить test matrix;
- стабилизировать performance.

Acceptance:

- VP9 capability matrix accurate;
- VP9 SDR/HDR samples покрыты тестами;
- ошибки parser'а или невозможные размеры не дают ложный `HardwareDecoderUnavailable`;
- strict reject происходит только для подтверждённо неподдерживаемого VP9 profile/format/resolution/HDR;
- frame pacing metrics стабильны.

## Phase 11: AV1 backend

Цель:

- добавить AV1 capability probing;
- добавить AV1 decode path через VA-API/cros-codecs или другой Rust-compatible backend;
- использовать AV1 sequence header parser из decode backend или adapter над ним;
- интегрировать stream selection.

Acceptance:

- AV1 stream выбирается только при hardware support;
- unsupported AV1 profile дает понятную ошибку;
- неполный или recoverable parse error sequence header не блокирует playback как hardware unsupported.

## Phase 12: H.264 backend and legacy path

Цель:

- добавить H.264 VA-API;
- прицел на старые Intel/i965;
- использовать H.264 SPS parser из decode backend или adapter над ним;
- подготовить будущий GLES renderer contract.

Acceptance:

- старые устройства с hardware H.264 могут воспроизводить SDR 8-bit H.264;
- SPS-derived profile/bit-depth/resolution покрыты golden tests;
- неуверенность parser'а не превращается в fatal capability rejection;
- Vulkan renderer работает там, где доступен;
- GLES path остается reserved, но архитектура готова.

## Phase 13: MP4/MOV/fMP4 demux

Цель:

- добавить MP4/MOV parsing/demux;
- подготовить fMP4 для DASH/HLS.

Acceptance:

- local MP4/MOV с hardware-supported video воспроизводится;
- no FFmpeg.

## Phase 14: YouTube service foundation

Цель:

- создать `service-youtube`;
- отделить временный `yt-dlp` adapter;
- ввести account/session/cookies storage contract;
- нормализовать stream candidates.

Acceptance:

- текущий YouTube MVP проходит через service boundary;
- app не знает о деталях extractor;
- stream selection capability-aware.

## Phase 15: Desktop integration

Цель:

- создать `desktop-integration`;
- добавить MPRIS D-Bus;
- связать с `PlayerCommand`/`PlayerSnapshot`.

Acceptance:

- KDE media widget видит rustiplayer;
- play/pause/seek работают через MPRIS;
- metadata отображается.

## Phase 16: Playlists/history/bookmarks

Цель:

- реализовать SQLite-backed library features;
- UI для истории/закладок;
- resume last position.

Acceptance:

- просмотр сохраняет прогресс;
- пользователь может открыть историю;
- bookmarks не зависят от конкретного source implementation.

## Phase 17: Network cache and resume

Цель:

- byte cache;
- cache index in SQLite;
- HTTP range resume;
- cleanup policy.

Acceptance:

- повторное открытие недавно смотренного media использует cache;
- interrupted download может продолжиться;
- cache limits configurable.

## Phase 18: Future codec expansion

Порядок:

1. H.265
2. VP8

Acceptance:

- каждый codec добавляется через capability model;
- stream selection не получает codec-specific hacks в app layer;
- H.265 использует VPS/SPS parser из decode backend или adapter над ним;
- VP8 не получает strict bitstream probing без доказанной необходимости;
- каждый новый probe на основе parser'а имеет golden tests и мягкое поведение при ошибке.

## Phase 19: Future platform expansion

Windows:

- DX12 render через `wgpu`;
- hardware decode backend позже.

macOS:

- Metal render через `wgpu`;
- VideoToolbox backend позже.

## Правило для каждого этапа

Перед реализацией этапа:

1. свериться с Context7 по внешним crate'ам;
2. описать короткий implementation plan;
3. не принимать важные архитектурные решения молча;
4. сохранить MVP working state;
5. сделать self-review после реализации.

Для этапов, которые добавляют codec probing или backend decode:

1. сначала проверить, есть ли parser в уже используемом backend crate;
2. не писать новый bit-level parser в `player-core`;
3. отделить подтверждённо неподдерживаемый stream от неуверенности parser'а;
4. покрыть реальные codec headers golden tests;
5. вручную проверить старый VP9 MVP после изменения общей capability логики.
