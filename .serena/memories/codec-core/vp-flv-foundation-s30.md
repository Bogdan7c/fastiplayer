# S30 VP/FLV codec foundation (2026-07-23)

Читайте вместе с `mem:core` и `mem:audio/core`.

## Владение и границы

- `codec-core::vp_configuration` владеет типизированным parser/normalizer VP Codec Configuration Record для VP8/VP9.
- Layout выбирается вызывающим явно через `VpCodecConfigurationLayout`; поддержаны чистый 8-byte record и доказанный FFmpeg Enhanced RTMP SequenceStart layout (4-byte version/flags prefix + record). Эвристического отбрасывания prefix нет.
- Parser проверяет version/flags, exact length, нулевой VP8/VP9 initialization-data size, profile/level, bit-depth/chroma и profile-depth-chroma matrix; ошибки представлены `VpCodecConfigurationError`.
- `VpCodecConfiguration` нормализует codec profile, chroma, full-range и H.273 colour fields в существующие codec-core contracts без container/network knowledge.
- `codec-core::vp8` владеет bounded structural VP8 packet-header probe. Он подтверждает keyframe только после валидного frame tag, partition length, sync code и ненулевых dimensions; malformed/ambiguous input не маркируется как keyframe.
- Общий adapter вызывает VP8 probe для `VideoCodec::Vp8`; mux/demux state, sequence-header lifecycle и network state остаются у будущего FLV demux/container слоя.

## Проверки

- Focused unit tests находятся в `crates/codec-core/src/vp_configuration.rs`, `crates/codec-core/src/vp8.rs` и adapter tests.
- Нормативные ориентиры: WebM VP Codec ISO Media File Format Binding; Adobe SWF/FLV specification; текущая FFmpeg реализация Enhanced RTMP VP sequence start.
