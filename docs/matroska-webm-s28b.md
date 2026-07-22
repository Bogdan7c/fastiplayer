# S28B: Matroska/WebM proof

## Профиль и границы владения

S28B закрывает finite local/progressive Matroska/WebM и finite serialized
`Initialization -> Media*` rows. Единственным container parser-ом остаётся exact
`symphonia-format-mkv 0.6.0`; проектный fork не дублирует EBML parsing и меняет
только доказанные upstream gaps.

Перед реализацией lifecycle сверялся с официальной документацией Symphonia 0.6:
`FormatReader::next_packet()` обязан вернуть `ResetRequired`, когда вызывающий код
должен перечитать track list и пересоздать декодеры. Exact crates.io archive fork-а
имеет SHA-256
`fb17713e134f5ad316c2690fa3104590ccc85842cdbcf82c3cd1a845cb08aa74`.

Владение разделено так:

- `symphonia-format-mkv-patch` читает EBML, lacing и `CodecState`, обновляет
  Symphonia track parameters и публикует `ResetRequired` до первого зависимого
  packet-а;
- `symphonia-demux` отображает этот lifecycle в neutral `TracksChanged`, владеет
  cue-aware `DecodePointBefore`, track/packet mapping и exact container capabilities;
- existing `demux-api` ordered adapter валидирует finite Init/Media lifecycle,
  sequence, empty rows, source errors и cancellation; он не знает EBML;
- `web-media-http` выбирает seekable Range либо forward-only non-Range byte source
  и не интерпретирует Matroska/WebM структуру;
- DASH manifest/addressing, network segment fetch, live/repeated init и
  discontinuity остаются владельцами S31/S34 и не добавлены в S28B.

## Автоматическая матрица доказательств

| Требование | Hermetic proof |
|---|---|
| Local без extension | `factory::tests::matroska::local_webm_proves_lacing_audio_timing_duration_and_codec_state_order` открывает generated WebM через `LocalFileSource` и signature sniff с `DemuxHints::none()`. |
| Progressive HTTP Range | `progressive_webm_range_reads_muxed_packet_timeline` обслуживает real muxed VP9+Opus WebM через bounded `206`, проверяет seekable source, A/V tracks, packet duration и media duration. |
| Progressive HTTP non-Range | `progressive_mp4_m4a_and_webm_open_with_real_hints_and_non_range_input` использует один forward-only `200` body и сохраняет честный `NotSeekable`. |
| Finite ordered rows | `ordered_webm_init_and_multiple_media_rows_preserve_packets_and_cancellation` передаёт один init и два media Cluster rows через production registry, читает все packets до clean EOF и отдельно проверяет cancellation. Capability рекламируется только для ISO BMFF, Matroska и WebM. |
| Cues и no-cues | `vp8_vp9_av1_and_cues_no_cues_decode_point_before_are_proven` выполняет текущий `DecodePointBefore` для обоих вариантов; focused cue-anchor/retry инварианты остаются в `symphonia_demuxer::tests::decode_point_before_matroska_*`. |
| None/Xiph/fixed/EBML lacing | Generated corpus формирует четыре независимых Block layout-а и проверяет exact девять frame payload-ов, PTS 0..800 ms и 100 ms duration каждого frame-а. |
| `CodecState` | Тот же corpus меняет VP9 state в `BlockGroup`: patch обновляет codec private, затем neutral boundary публикует ровно один `TracksChanged`, и только после него отдаёт packet `0x50`. Ordered wrapper одновременно обновляет последующий public `tracks()` snapshot. Direct patch test проверяет замену codec-specific extra data без потери независимых video extra-data records. |
| VP8/VP9/AV1 | `vp8_vp9_av1_and_cues_no_cues_decode_point_before_are_proven` открывает отдельный generated TrackEntry каждого codec-а и проверяет точный neutral codec ID. Decoder/capability semantics AV1 остаются у `codec-core`. |
| Opus/Vorbis | Main generated corpus содержит valid OpusHead и минимальный Matroska Xiph Vorbis private layout; оба audio track-а обязаны появиться с точными codec IDs, sample rate и channels. Real progressive corpus дополнительно покрывает VP9+Opus packets. |
| Timestamps/duration/seek | Main corpus проверяет packet PTS/duration, authoritative Segment duration и cue/no-cue `DecodePointBefore`; Range test сохраняет transport parity. |
| Malformed/truncated | `malformed_lacing_and_declared_payload_truncation_are_not_clean_eof` отличает invalid fixed lacing и обрезанный declared Cluster payload от EOS. Direct patch tests различают exact document boundary, partial EBML header, child payload за physical document end и EOF внутри объявленного parent у forward-only источника с неизвестной общей длиной. Generic `UnexpectedEof` mapping других container readers не расширялся. |
| Cancel/sniff | Ordered WebM test отменяет source во время finite open; local test открывается без extension; registry tests сохраняют bounded truncated/no-match/cancel taxonomy и приоритет EBML signature над hints. |

## Dependency patch provenance и removal gate

Fork зарегистрирован в root `[replace]`, `workspace.exclude`, CI patch matrix и
`docs/dependency-patches.toml`. Он сохраняет upstream MPL-2.0 metadata и отдельный
lockfile. Удаление допустимо только после released upstream-эквивалента обоих
контрактов — `CodecState -> ResetRequired` ordering и structural EBML truncation —
и после direct patch suite, S28B hermetic matrix и реальной media regression.

## Осознанные ограничения

- Ordered inputs остаются finite и non-seekable; seek доказывается на local/Range
  byte source. Repeated init, live/DVR, refresh и manifest composition принадлежат
  S31+.
- Sequential `CodecState` transition доказан и fail-safe. Seek через границу
  `CodecState` не включён в compatibility claim: для него нужен отдельный
  authoritative `CueCodecState` restore contract; до такого proof profile не должен
  обещать random access через dynamic-config boundary.
- Test fixture проверяет container/demux contracts и codec identities, а не
  декодирует synthetic payload bytes. Реальный decode diversity остаётся в
  opt-in `scripts/media-regression.sh` и codec/backend suites.
- Второй EBML/Matroska parser, FFmpeg demux fallback, новые codecs и DASH network
  composition не добавлялись.
