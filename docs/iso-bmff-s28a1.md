# S28A1: доказательство classic ISO BMFF

Этот документ фиксирует только конечные classic MP4/M4A/MOV/3GP-файлы. Fragmented
MP4/CMAF и ordered segments принадлежат следующим секциям S28A и здесь намеренно не
считаются завершёнными.

## Владение и границы

- `symphonia-demux` владеет bounded signature sniff, concrete Symphonia reader и
  преобразованием container tracks/packets в `media-core`.
- `web-media-http` владеет выбором seekable Range либо forward-only non-Range source.
  Он не интерпретирует MP4 atoms, timestamps или codec private data.
- `symphonia-format-isomp4-patch` остаётся единственным владельцем project fixes для
  `ctts`, `stss`, display matrix, color/HDR metadata и MOV PCM packet coalescing.
- `codec-core` остаётся владельцем `avcC`/`hvcC`/`av1C` parsing и packetization.

## Автоматическая матрица

| Требование | Hermetic proof |
|---|---|
| Local MP4 и M4A | `classic_iso_bmff_local_and_range_preserve_timing_seek_and_codec_private` открывает один и тот же corpus через `LocalFileSource` и S21 registry. |
| Progressive HTTP Range | Тот же тест обслуживает реальные bounded `206` ranges, проверяет seekability, duration, codec private, исходные track timestamps/duration, packet data и повторное чтение после seek. |
| Progressive HTTP non-Range | `progressive_mp4_m4a_and_webm_open_with_real_hints_and_non_range_input` открывает MP4/M4A из единственного `200` response и проверяет честный `NotSeekable`. |
| MOV и 3GP | `mov_and_3gp_brands_open_by_signature_without_extension` переводит весь `ftyp` в `qt  ` либо `3gp6`, не создавая второй parser или дублирующий `moov`/`mdat` corpus. |
| Sniff без extension и конфликтующий hint | Предыдущий тест передаёт `DemuxHints::none`; `iso_bmff_signature_overrides_conflicting_wave_hint` доказывает приоритет `ftyp` над ложными Wave hints. |
| Truncated, malformed и cancel | `factory::tests::truncated_and_no_match_are_distinct` различает truncated `ftyp`, no-match и отменённый ISO BMFF probe; `local_probe::tests::unexpected_eof_is_malformed_probe_failure` сохраняет truncated header как probe failure; `symphonia_demuxer::tests::decode_error_from_format_reader_is_parse_error_without_retry` сохраняет malformed parser error. Generic progressive cancellation/backpressure проверяется в `demux-api::progressive::tests`. |
| `avcC`/`hvcC`/`av1C` и packetization | Новый local/Range test проверяет непустой codec private на реальном H.264/AAC corpus. Точные structural варианты остаются в `codec-core::{h264,h265,av1}` и `symphonia-demux::{packet_mapper,track_mapper}` tests. |
| PTS/DTS/duration/seek | Новый local/Range test проверяет transport parity и исходные track units. Signed/unsigned composition offsets остаются в patch `ctts` tests; sync-safe seek — в `stss` tests и opt-in H.264/H.265 real-media regressions. |
| Metadata/color/orientation | Per-track mapping закреплён `demuxer_maps_per_track_display_orientation_metadata`, `demuxer_maps_mp4_per_track_hdr_color_metadata`, `mp4_color_metadata_wins_over_matroska_color_fallback` и synthetic patch tests для `colr`/`mdcv`/`clli`/`tkhd`. |
| MOV PCM | Sample-entry и packet grouping закреплены `lpcm_v2_sample_entry_preserves_frames_per_packet` и `pcm_packet_span_*`; mapper/player не компенсируют однофреймовые samples. |

## Ручные реальные regressions

Default tests не читают `test-assets/` и не включают реальные media через
`include_bytes!`. Для явно выбранного пользователем файла остаётся
`scripts/media-regression.sh`: сценарии `h264-avcc`, `h264-bframes-pts-dts`,
`h264-signed-ctts`, `h265-mov-sync-sample`, `h265-hvcc` и `direct-http-range`.

Этот opt-in слой нужен для разнообразия encoder/container quirks, но не заменяет
hermetic completion gate выше.

## Не входит в S28A1

- `moof`/`traf`/`trun`, `tfdt` и fragmented duration/seek/truncation;
- init/media ordered segments, repeated init и discontinuity;
- HLS, DASH, ISM/MSS, live и `avc3`.
