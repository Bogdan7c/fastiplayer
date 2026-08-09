# HLS live `avc3` и install-ready topology (2026-08-10)

## Инцидент и root cause

- Acceptance row `hls-live-dvr` (BBC/Akamai testcard) не открывалась: локальный `symphonia-format-isomp4` распознавал `avc1`, но не `avc3`, поэтому valid fMP4 video sample entry не становился H.264 track.
- У exact source минимальный `avcC` равен `01 4d 40 1f ff e0 00`: он задаёт четырёхбайтовый NAL length prefix, но не содержит SPS/PPS. Для `avc3` это валидно, потому что SPS/PPS приходят внутри media samples; строгий `avc1` обязан по-прежнему отклонять такой record.
- После исправления demux второй app-level барьер ошибочно требовал replay события `TracksChanged`. `HlsLiveComponentDemuxer` уже применяет initial topology во время bootstrap и передаёт наружу непустой `tracks()` snapshot, поэтому следующим законным событием может быть `Packet`.

## Владельцы и boundaries

- `symphonia-format-isomp4-patch` владеет точным различением sample entry `avc1`/`avc3` и публикует per-track raw tag `rustiplayer.video.h264.parameter_sets_in_band=true` только для `avc3`.
- `symphonia-demux` переводит tag в нейтральный `VideoPacketFraming::LengthPrefixedWithInBandParameterSets`. Mapper применяет это evidence только к video track с exact codec id `V_MPEG4/ISO/AVC`; чужой tag не может перекрасить VP9 или другой codec.
- `codec-core` разделяет строгий `parse_avc_decoder_configuration_record` (`avc1`, SPS/PPS обязательны) и `parse_avc3_decoder_configuration_record` (`avc3`, in-band parameter sets допустимы). `H264Packetization` сохраняет это различие typed-вариантом.
- `player-core` преобразует neutral framing в decoder stream config. Missing/malformed `avc3 avcC` остаётся typed `UnsupportedVideoCodec` до configure decoder-а.
- `video-vaapi` принимает минимальный `avc3 avcC`, переводит length-prefixed sample с in-band SPS/PPS/IDR в Annex-B без скрытой подмены lifecycle policy. `video-ffmpeg` передаёт тот же `avcC` как extradata.
- `app-egui::web_media_hls_open` определяет install readiness по authoritative `demuxer.tracks()` state, а не по ритуалу получения конкретного события. Уже готовый snapshot не потребляет первый packet; packet при пустой topology, EOS и deadline остаются fail-closed.

## Инварианты и текущий source snapshot

- `avc1` contract не ослаблен: configuration record без SPS/PPS отклоняется.
- `avc3` не угадывается по URL/расширению; его доказывает ISO BMFF sample entry.
- Любой live HLS обязан иметь непустой authoritative track snapshot до Installed, не только deferred-codec layout.
- На 2026-08-10 master публикует 6 video variants `avc3` (192x108 .. 896x504) и alternate audio `mp4a.40.5` (HE-AAC). Текущий Rustiplayer audio profile поддерживает AAC-LC, поэтому эта acceptance row доказывает video/live/DVR без обещания звука; HE-AAC требует отдельного decoder scope.
- Выбранный 896x504 media playlist имел 234 segment-а и sliding window 898.560 s.

## Проверки

- `crates/web-media-hls/tests/runtime.rs::avc3_fmp4_map_preserves_framing_and_emits_video_packet`: HTTP -> HLS -> fMP4 patch -> neutral track + video packet.
- `crates/player-core/src/session/tests/capability_selection.rs::hls_avc3_sample_with_in_band_parameter_sets_reaches_presentation`: track + real-shaped SPS/PPS/IDR sample -> fake decoder -> `video_frames_presented == 1`.
- App barrier tests покрывают bootstrap snapshot без packet consumption, metadata/temporary events, packet-before-topology и EOS.
- Codec/backend tests покрывают strict `avc1`, minimal `avc3`, requirement probe, VA-API Annex-B feed и FFmpeg extradata.
- Реальный release smoke exact URL прошёл на `ffmpeg-host-upload-wgpu` и VA-API DMA-BUF paths до повторяющихся `Presenting scheduled video frame`.
- Финальные проверки: affected all-target tests, direct ISO patch tests (91), FFmpeg feature tests, strict Clippy `-D warnings`, Rust 1.92 check, dependency-patch CI, fmt, refactor guardrails и `git diff --check` — PASS.

Связанные memories: `mem:codec-core/h264`, `mem:symphonia-demux/h264`, `mem:dependency-patches/core`, `mem:media-services/hls-live-s33-2026-07-24`, `mem:testing/web-media-playlist-acceptance-2026-08-04`.
