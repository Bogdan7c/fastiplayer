# S21C neutral web-media capability composition и selection (2026-07-21)

## Ownership и dependency boundary

- Новый workspace crate `web-media-playback-plan` владеет pure pre-I/O planning boundary. Normal dependencies: только `web-media-core`, `demux-api`, `codec-core`, `capability-core`, `audio-core`; concrete service/provider/source/demuxer/decoder/player/app/UI dependencies запрещены refactor guardrail-ом.
- `web-media-core` остаётся std-only. `SelectionRequest::Exact` теперь принимает `ExactSelectionIdentity`, которая атомарно несёт snapshot-local `CandidateIdentity` и refresh-stable `SemanticIdentity` одной source lineage.
- `demux-api::DemuxInputCapabilities` получил immutable `union`, `intersection`, `intersects` для нейтрального пересечения transport output и demux input shapes.

## Snapshot и candidate contracts

- Composition передаёт `TransportCapabilitySnapshot`, `DemuxCapabilitySnapshot`, existing `SystemCapabilities` и S20 `AudioDecodeCapabilitySnapshot` через read-only `PlaybackCapabilitySnapshot`.
- Transport registration описывает `TransportFamily -> DemuxInputCapabilities`; несколько registrations одной family объединяются. Demux registration аналогично описывает `ContainerFamily -> DemuxInputCapabilities`.
- `PlanningCandidate` принимает S19 `CandidateDescriptor`, shape-matching `CandidateRuntimeRequirements`, named `CandidateQualityScore`; admission проверяет exact layout shape, known/non-excluded transport+container, video/audio codec correspondence, SDR/HDR consistency и совпадение descriptor resolution с `VideoDecodeRequirement`.
- Static rejected/profile-excluded inventory не является input planner-а и отсутствует в `PlaybackPlanningError`; operational provider/open errors также отсутствуют. Их владельцы остаются upstream normalization и downstream open соответственно.
- `PlanningCandidateSnapshot` связывает всех candidates с одной source/extraction generation и отвергает duplicate exact identities.

## Pure selection semantics

- `plan_playback` проверяет `Muxed`, `Separate`, `VideoOnly`, `AudioOnly`, при separate локализует rejection на exact video/audio component.
- Capability rejection layers не схлопываются: transport family absent; demux container absent либо transport/demux input-shape mismatch; existing detailed video rejection; S20 audio unavailable/query rejection.
- BestPlayable сначала оценивает полный inventory, поэтому несовместимый candidate не блокирует совместимый и остаётся в diagnostics.
- Ordering: full capability pass -> HDR bucket -> explicit video codec order -> S20Q preferred height exact/lower/higher/missing -> explicit container order -> descending service quality score -> semantic identity -> exact identity.
- `SdrOnly` typed-исключает HDR; `PreferHdrWhenAvailable` ранжирует playable HDR выше SDR; unknown dynamic range не угадывается как SDR.
- Exact не выполняет скрытый semantic rematch: source mismatch, stale extraction generation, missing exact ID и changed semantic attributes — разные `PlaybackPlanningError` variants.
- `PlaybackPlan` сохраняет exact+semantic identity, layout kind и cloned matched `SupportedVideoOutput` proof для video layout-а.

## Focused evidence и workflow

- Tests: absent transport/demux/video/audio одним muxed candidate; четыре layout shape; transport/demux input mismatch; preferred exact/lower/higher; HDR/codec/container tie-break; incompatible-neighbor continuation; stale/semantic-changed Exact; static admission; resolution mismatch; unknown dynamic range.
- `web-media-playback-plan` и ранее пропущенный `web-media-transport-api` добавлены в blocking coverage inventory; новый crate добавлен в cargo-machete/CI workspace inventory.
- PASS: focused tests (`web-media-core`, `demux-api`, `web-media-playback-plan`), all-features workspace tests, focused strict Clippy, Rust 1.96 locked workspace check, Rust 1.92 locked MSRV check, strict workspace rustdoc, format/toolchain/guardrail suite, cargo-machete, diff check и Serena diagnostics.
- Full workspace strict Clippy остаётся заблокирован pre-existing `ui-artwork-egui` `items_after_test_module`. Dependency advisory report показывает pre-existing unmaintained `audiopus_sys` и `ttf-parser`; новый crate не добавляет external dependencies.

## Current limitation

- S21C создаёт planning boundary и тестовые immutable snapshots, но ещё не подключает S19 `service-ytdlp` inventory или concrete transport/demux registries к production playback. Provider/service-specific mapping и exact open принадлежат следующим vertical-slice milestones (S22+), а не pure planner-у.

Related: `mem:media-services/core`, `mem:media-services/ytdlp-hdr-selection-s16`, `mem:audio/core`, `mem:demux-api/core`, `mem:media-services/web-transport-s21t-2026-07-21`, `mem:config/schema-v7-quality-preference-2026-07-21`.