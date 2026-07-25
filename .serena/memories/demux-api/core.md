## S36P4/P5 deferred receipted Smooth runtime (2026-07-25)

- `ProgressiveDemuxer::new_deferred_receipted_seekable` additively combines worker-deferred concrete open with the existing generation-fenced async seek receipt vocabulary. Preview planning is pure; seek execution and replacement stay on the worker. Existing constructors and non-seekable behavior are unchanged.
- `web-media-smooth` uses this boundary to build stable video/audio adapters and transactionally swap a complete `CompositeAvDemuxer`; failure preserves the active pair. Full contract: `mem:media-services/smooth-vod-runtime-s36p4-p6-2026-07-25`.

## S36F3A presentation-window ordered boundary (2026-07-25)

- `demux-api` now has a parallel window-aware ordered segment/source outcome contract; existing `OrderedSegment` and `DemuxInput` are unchanged. `symphonia-demux` opens each windowed ISO media fragment as isolated shared `init + one media` bytes and tags packets from exact segment provenance, never by overlapping PTS/read-ahead state.
- Strict init/sequence/discontinuity/stable-track/readiness/EOS/cancellation rules and player transport-only handoff: `mem:media-services/presentation-window-transport-s36f3a-2026-07-25`.

## S32B exact required-container open (2026-07-23)

- `DemuxRegistry::open_required_container` preserves normal registry sniff/replay/open but rejects a content-proven different container through typed `DemuxOpenError::UnexpectedContainer`. HLS composition injects a reusable registry and never selects TS/fMP4 from extension or MAP presence. Full consumer contract: `mem:media-services/hls-vod-s32b-2026-07-23`.

## S31 deferred adaptive open (2026-07-23)

- Existing finite `OrderedSegmentSource` сохранён. Новый adaptive poll-source живёт в `web-media-adaptive`; `BlockingOrderedSegmentAdapter` используется только внутри `ProgressiveDemuxer::new_deferred`, который переносит initial segment readiness, registry sniff/open и parser reads на worker и возвращает player-owner-у S21R `TemporarilyUnavailable`. Seekable inner typed rejected, а не молча downgraded. Полный handoff: `mem:media-services/adaptive-transport-s31-2026-07-23`.

## S29 ordered discontinuity + MPEG-TS factory (2026-07-22)

- `OrderedSegment` теперь несёт typed `OrderedSegmentDiscontinuity::{Continuous, StartsNewTimeline}`. MPEG-TS использует marker для decoder/timestamp reset и публикует `TracksChanged` перед зависимым packet; Symphonia finite adapter fail-closed возвращает `DiscontinuityRequiresSessionReset`, а не молча игнорирует marker.
- Новый `mpeg-ts-demux::MpegTsDemuxFactory` регистрирует exact container `mpeg-ts`, extension `ts`, MIME `video/mp2t` и честные ByteSource/ByteStream/OrderedSegments capabilities. 192-byte M2TS не рекламируется. Подробности: `mem:mpeg-ts-demux/core`.

# demux-api — S21 neutral demux boundary

## Роль

`crates/demux-api` владеет typed composition поверх существующего `media_core::Demuxer`. Crate остаётся neutral: ему разрешены только `anyhow`, `bytes`, `media-core`, `source-core`, `thiserror`, `tracing`; concrete container backends, player, services, UI и GPU запрещены guardrail-ом.

## Registry и probe/open

- `DemuxInput` различает `ByteSource`, sequential byte stream и ordered segment source; capability объявляется через `DemuxInputCapability`/`DemuxInputCapabilities`.
- `DemuxHints` содержит typed container/extension/MIME hints. Hints являются evidence, но не заменяют content sniff.
- `DemuxSniffBudget` требует ненулевые byte/segment/time bounds.
- `DemuxFactory` публикует immutable descriptor: factory ID, container registrations и fixture IDs. После S28A2P каждый `DemuxContainerRegistration` владеет exact `DemuxInputCapabilities`; `DemuxFactoryDescriptor::input_capabilities()` вычисляет только factory-wide union для prefilter-а. После probe registry обязательно перепроверяет exact пару `(matched container, input capability)`, а planner проецирует capabilities из конкретной registration. После S28B finite ordered support зарегистрирован для ISO BMFF, Matroska и WebM и не протекает в Ogg/WAV/другие соседние rows; neutral API при этом не менялся.
- `DemuxRegistry` отклоняет duplicate factory/container ownership, выполняет bounded probe и возвращает typed no-match/ambiguity/probe/open failures.
- После sniff input восстанавливается без потери bytes: seekable source возвращается к исходной позиции, non-seekable byte/stream input получает prefix replay wrapper, ordered segments replay-ятся с исходными boundaries.
- Input replay owner: `src/registry/input_replay.rs`; registry selection/registration owner: `src/registry.rs`.

## Neutral A/V composition

- `CompositeAvDemuxer` владеет двумя boxed `Demuxer + Send`, explicit selected inner video/audio track IDs и stable collision-free public IDs.
- Default public remap сохраняет video ID и меняет audio только при collision; compatibility callers могут передать explicit public IDs.
- Packet output фильтруется по exact selected tracks, remap-ится и interleave-ится по presentation timestamp; one-side EOF не завершает вторую сторону.
- `TracksChanged` пересобирает merged snapshot, сохраняя public IDs; metadata остаётся video-primary, audio заполняет только пропуски.
- Seek сохраняет старые runtime signatures: video получает исходный request, audio получает Accurate для DecodePointBefore; partial failure сохраняется в downcastable `CompositeComponentSeekError`.
- Duration fallback: selected video track → selected audio track → video demux duration → audio demux duration.
- Bounded `CompositeComponentLeadPolicy` живёт в `src/composite/policy.rs`; S21R применяет timestamp lead после появления comparable PTS и bootstrap packet/byte caps до него.
- Readiness/lead accounting вынесен в `src/composite/readiness.rs`: не больше одного validated pending packet на component, oversized pending payload даёт typed `CompositePendingPacketTooLargeError`, required unavailable hints объединяются по minimum earliest retry, EOF component больше не ограничивает живой peer, общий EOS публикуется только после terminal state обеих required components.
- `media_core::Demuxer` теперь required `next_event`-only; generic `next_packet` отсутствует. Runtime owner: `src/composite.rs`; track validation/remap/static metadata: `src/composite/track_layout.rs`.

## Проверки

Focused tests находятся в `src/registry/tests.rs` и `src/composite/tests.rs`: hints agree/disagree, duplicates, seekable/streaming/segments replay, truncated/cancel/no-match, H.264+AAC fake composition, collisions, one-side EOF, partial seek failure и `TracksChanged`.

Связанные memories: `mem:media-services/core`, `mem:symphonia-demux/core`.

## S21C capability composition (2026-07-21)
- `DemuxInputCapabilities` получил intent-level set operations `union`, `intersection`, `intersects`; neutral planner использует immutable demux snapshot для container/input-shape rejection до I/O.
- Детали: `mem:media-services/web-playback-planner-s21c-2026-07-21`.


## S22 progressive demux boundary (2026-07-22)

- `DemuxInput::streaming_source` adapts a cancellation-aware S21T `StreamingByteSource` to the byte-stream registry input.
- `ProgressiveDemuxer` is the neutral owner of a separate blocking inner-demux worker. Player-facing `next_event` only polls a bounded event/encoded-byte queue and returns `TemporarilyUnavailable` when empty.
- This worker is required because exposing `WouldBlock` from the middle of a concrete container parser can lose partially consumed parser state.
- Full queue backpressures the worker; oversized packets fail typed; drop requests stop/cancellation without joining on player owner. A RAII completion guard prevents endless readiness after backend panic.
- See `mem:media-services/progressive-http-s22-2026-07-22` for the concrete HTTP path.
