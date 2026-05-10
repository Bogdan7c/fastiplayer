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

Статус: реализовано.

Цель:

- создать `render-core`;
- создать/переименовать `render-wgpu`;
- вынести `app-egui/src/render.rs`;
- описать `RenderableFrame` и `RenderCapabilities`.

Acceptance:

- app shell не знает детали NV12 renderer;
- renderer capabilities участвуют в stream selection;
- текущий NV12 VP9 path работает.

## Phase 8.5: SDR color pipeline prep

Подробный план: [09. Phase 8.5 SDR Color Pipeline Prep](09-phase-8-5-sdr-color-pipeline-prep.md).

Цель:

- сохранить текущий рабочий SDR VP9/NV12 путь;
- не менять намеренно визуальный SDR результат;
- заменить hardcoded `NV12 + BT.709 limited SDR` assumptions на явный renderer color pipeline contract;
- добавить typed color metadata path от decoder boundary до renderer uniforms;
- заложить SDR/RGB adjustments и active color path diagnostics;
- сохранить zero-copy DMA-BUF import как целевой path;
- подготовить Phase 9 VP9/P010 readiness и Phase 10 HDR renderer без превращения `nv12_to_rgba.wgsl` в универсальный HDR shader.

Acceptance:

- SDR VP9/NV12 playback работает как до refactor;
- `BT.709 limited` conversion даёт тот же или объяснимо близкий результат;
- `RenderCapabilities` не объявляет HDR/P010 support преждевременно;
- UI/telemetry может показать active color path вроде `NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm`;
- unit tests покрывают metadata -> uniforms mapping;
- shader/source tests проверяют, что NV12 UV order не сломан;
- zero-copy decoded NV12 DMA-BUF import не заменён CPU color conversion path.

## Phase 9: Full VP9 completion

Подробный план: [09. Phase 9 Full VP9 Completion](09-phase-9-vp9-completion.md).

Цель:

- закрыть текущие ограничения VP9 до уровня полной typed capability/selection модели;
- сохранить рабочий VP9 Profile 0 SDR/NV12 production path;
- распознавать все VP9 profiles, bit depths и chroma variants;
- поддержать VP9 Profile 2 10-bit 4:2:0 как P010/HDR readiness boundary, если hardware позволяет;
- честно отклонять VP9 12-bit, 4:2:2 и 4:4:4 variants с typed reasons;
- добавить layered VP9/WebM color metadata resolver;
- доказать `P010 + HDR metadata + zero-copy render boundary` для Phase 10 без включения HDR playback.

Acceptance:

- VP9 Profile 0 SDR playback работает как до refactor;
- VP9 Profile 0/1/2/3 распознаются;
- VP9 Profile 1/3 rejected как unsupported chroma, а не generic hardware failure;
- VP9 12-bit rejected как unsupported bit depth;
- VP9 Profile 2 10-bit 4:2:0 на поддержанном hardware доходит до P010 zero-copy boundary;
- production HDR playback всё ещё rejected до Phase 10 с понятной причиной;
- P010 path не имеет CPU upload/readback fallback;
- metadata resolver покрыт conflict tests;
- capability rejection reasons typed и codec-agnostic;
- SDR VP9/NV12 regression покрыт unit/manual tests.

## Phase 10: HDR-to-SDR baseline

Подробный план: [10. Phase 10 HDR-to-SDR Baseline](10-phase-10-hdr-to-sdr-baseline.md).

Цель:

- использовать готовый Phase 9 `P010 + HDR metadata + zero-copy` render boundary;
- добавить отдельный P010/HDR shader path;
- реализовать HDR-to-SDR conversion по ITU-R BT.2446 Method C;
- поддержать PQ и HLG input;
- вывести SDR BT.709 через explicit shader OETF;
- сохранить SDR path из Phase 8.5/9 стабильным.

Acceptance:

- HDR input не отображается как washed-out SDR;
- P010 используется только через zero-copy path;
- `supports_hdr_to_sdr = true` появляется только после рабочей BT.2446-C реализации;
- `supports_native_hdr_output = false`;
- PQ и HLG покрыты tests;
- UI показывает active HDR color path;
- optional mastering/CLL/FALL defaults видны в diagnostics;
- SDR VP9/NV12 path не ломается;
- HDR renderer fail-closed, без SDR/CPU fallback.

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
