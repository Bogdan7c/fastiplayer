# demux-api — S21 neutral demux boundary

## Роль

`crates/demux-api` владеет typed composition поверх существующего `media_core::Demuxer`. Crate остаётся neutral: ему разрешены только `anyhow`, `bytes`, `media-core`, `source-core`, `thiserror`, `tracing`; concrete container backends, player, services, UI и GPU запрещены guardrail-ом.

## Registry и probe/open

- `DemuxInput` различает `ByteSource`, sequential byte stream и ordered segment source; capability объявляется через `DemuxInputCapability`/`DemuxInputCapabilities`.
- `DemuxHints` содержит typed container/extension/MIME hints. Hints являются evidence, но не заменяют content sniff.
- `DemuxSniffBudget` требует ненулевые byte/segment/time bounds.
- `DemuxFactory` публикует immutable descriptor: factory ID, capabilities, container registrations и fixture IDs.
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
- Bounded `CompositeComponentLeadPolicy` вынесена в `src/composite/policy.rs`; S21 использует один pending packet. Реальная readiness/`next_event`-only read migration остаётся S21R.
- Runtime owner: `src/composite.rs`; track validation/remap/static metadata: `src/composite/track_layout.rs`.

## Проверки

Focused tests находятся в `src/registry/tests.rs` и `src/composite/tests.rs`: hints agree/disagree, duplicates, seekable/streaming/segments replay, truncated/cancel/no-match, H.264+AAC fake composition, collisions, one-side EOF, partial seek failure и `TracksChanged`.

Связанные memories: `mem:media-services/core`, `mem:symphonia-demux/core`.