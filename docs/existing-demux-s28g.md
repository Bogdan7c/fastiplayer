# S28G: existing-demux hardening gate

## Результат и границы

S28G сводит завершённые S28A/B/C в одну reuse foundation без новой feature
logic. Runtime API, parser behavior, transport behavior и patch semantics не
меняются. Gate закрепляет уже существующие owner-ы, exact capabilities,
hermetic fixtures и failure taxonomy.

Границы владения остаются такими:

- `demux-api` владеет neutral `DemuxInput`, bounded sniff/replay,
  `DemuxRegistry`, exact `(container, input)` capability validation и
  `CompositeAvDemuxer`;
- `symphonia-demux` владеет Symphonia adapter-ом, neutral event/error mapping,
  seek verification, finite ordered wrapper и container signature mapping;
- exact `symphonia-format-isomp4`, `symphonia-format-mkv` и
  `symphonia-format-caf` patches владеют container packet parsing;
- `web-media-http` владеет только progressive HTTP Range/non-Range transport и
  не интерпретирует container bytes;
- `player-core` получает готовый neutral demuxer и не открывает concrete
  container backend напрямую.

## Input capability matrix

Factory descriptor публикует capabilities у exact registration row, а не у
factory-wide container union.

| Container row | Seekable bytes | Streaming bytes | Finite ordered segments |
|---|---:|---:|---:|
| `iso-bmff` | да | да | да |
| `matroska` | да | да | да |
| `webm` | да | да | да |
| `ogg` | да | да | нет |
| `caf` | да | да | нет |
| `wave` | да | да | нет |
| `aiff` | да | да | нет |
| `flac` | да | да | нет |
| `mpeg-audio` | да | да | нет |

`OrderedSegments` здесь означает только конечную последовательность: ровно один
непустой `Initialization`, затем непустые `Media` с возрастающим sequence.
Repeated init, discontinuity, live/DVR и manifest fetching не входят в S28G.
Ordered demuxer всегда честно публикует `NotSeekable`.

Focused proof:

- `descriptor_declares_ordered_segments_only_for_proven_fragmented_containers`;
- `registry_validates_the_exact_container_and_input_pair`;
- `ordered_fmp4_opens_without_hint_and_reads_multiple_media_fragments`;
- `ordered_webm_init_and_multiple_media_rows_preserve_packets_and_cancellation`;
- `exact_matroska_doctype_opens_local_and_ordered_inputs`.

Последний test использует настоящий `DocType=matroska`. Generated WebM больше не
выдаётся за доказательство exact Matroska registration.

## Local, progressive и signature evidence

| Surface | Evidence |
|---|---|
| Local ISO BMFF | `classic_iso_bmff_local_and_range_preserve_timing_seek_and_codec_private`, MOV/3GP brand tests и `factory::tests::fragmented_isomp4` |
| Progressive ISO BMFF | тот же generated MP4/M4A corpus через HTTP `206` Range и `200` non-Range в `web-media-http/tests/progressive_containers.rs` |
| Local Matroska/WebM | exact Matroska test выше и `local_webm_proves_lacing_audio_timing_duration_and_codec_state_order` |
| Progressive WebM | `progressive_webm_range_reads_muxed_packet_timeline` и общий non-Range test |
| Current audio containers | S28C local factory matrix и `web-media-http/tests/progressive_audio_containers.rs` для Range/non-Range |

Content signature авторитетнее conflicting extension/container hint. Registry
sniff ограничен `DemuxSniffBudget`; seekable input возвращается к исходной
позиции, streaming input получает prefix replay, ordered input — replay уже
прочитанных segment boundaries. Truncated, cancelled, no-match, ambiguous probe
и backend open failure остаются разными outcomes.

## Neutral composite A/V

Generic composition остаётся у `demux-api::CompositeAvDemuxer`:

- exact выбранные inner track IDs получают стабильный collision-safe public
  mapping;
- packets interleave-ятся по PTS, а EOF одной стороны не завершает вторую;
- `TracksChanged` пересобирает snapshot без смены public IDs;
- video-primary metadata дополняется только отсутствующими audio values;
- partial seek failure сохраняет component identity и уже выполненный video
  seek;
- bounded readiness/lead accounting удерживает не больше одного validated
  pending packet на component.

`separate_progressive_mp4_and_m4a_compose_through_neutral_av_demuxer` доказывает
реальный progressive H.264/AAC composition path. `DualStreamDemuxer` остаётся
тонким compatibility/admission adapter-ом и не дублирует generic composition.

## Seek, non-seekable и event semantics

- Seekable local/Range paths сохраняют container/decode-safe actual landing;
  финальный pre-roll/drop принадлежит player layer.
- Non-Range и ordered paths отклоняют seek через typed `NotSeekable` до backend
  mutation. S28C streaming tests после rejection продолжают чтение до clean EOS.
- `ResetRequired` отображается в `TracksChanged`; updated public `tracks()`
  snapshot виден до зависимого packet-а.
- `TemporarilyUnavailable` остаётся отдельным nonterminal readiness event и не
  сливается с EOF/error/track mutation.
- Clean EOF, fatal structural parse error и cancellation остаются различимыми.
- Defensive generic `IoError(UnexpectedEof) -> EndOfStream` compatibility
  fallback в `symphonia-demux` не расширялся. Exact ISO BMFF и Matroska patches
  сами отличают boundary EOF от container-owned truncation.

Focused owners: `demux-api/src/composite/tests.rs`,
`symphonia-demux/src/symphonia_demuxer/tests.rs`, S28A/B/C factory tests и
progressive container integrations.

## Parser ownership и bounded Matroska exception

`symphonia-format-mkv-patch` — единственный Matroska/WebM packet/container
parser. Только он разбирает `Cluster`, `Block`, lacing, `CodecState` ordering и
packet payload.

`symphonia-demux/src/matroska_metadata.rs` является осознанным bounded fail-open
structural metadata/cue indexer-ом. Он читает только:

- `Tracks`/`TrackEntry`/`Video`/`Colour` для недостающей neutral video metadata;
- `Info/TimestampScale`, `SeekHead` и `Cues` для bounded seek-anchor lookup.

Indexer не спускается в `Cluster` и не разбирает `Block`, lacing или packet
payload. Test `cluster_payload_is_opaque_to_bounded_metadata_and_cue_indexer`
кладёт ложные `Tracks`/`Cues` внутрь `Cluster` и доказывает, что они остаются
невидимыми indexer-у.

`scripts/check-refactor-guardrails.py` закрепляет boundary двумя способами:

- запрещает alternative parser dependencies у `symphonia-demux`;
- проверяет production parser declarations в `symphonia-demux/src`, но не
  запрещает полезные слова в документации, assertions и test-only corpus
  builders.

Перенос bounded indexer-а в format patch или расширение до packet parsing требует
отдельной design/ownership session и не является gate-правкой.

## Current patches и regression inventory

Machine-readable provenance и removal gates находятся в
`docs/dependency-patches.toml`. Для S28G критичны:

- `symphonia-format-isomp4:0.6.0`: classic/fMP4 timing, sync-safe seek,
  truncation, duration authority, color/orientation и MOV PCM fixes;
- `symphonia-format-mkv:0.6.0`: `CodecState -> ResetRequired` ordering и
  structural EBML truncation;
- `symphonia-format-caf:0.6.0`: forward-only CAF open/read и exact truncation.

Patch removal, dependency bump, FFmpeg/libavformat fallback и новый parser не
входят в S28G. Проверка inventory: `scripts/ci-checks.sh dependency-patches` плюс
direct `--locked` tests каждого format patch.

## Fixture inventory

Factory descriptor содержит exact checked evidence identities:

| Session | Fixture IDs |
|---|---|
| S28A | `symphonia/mp4-h264-aac`, `symphonia/generated-fmp4-s28a` |
| S28B | `symphonia/webm-vp9-opus`, `symphonia/generated-webm-s28b`, `symphonia/generated-matroska-ordered-s28b` |
| S28C | `symphonia/s28c-ogg-opus`, `symphonia/s28c-caf-pcm`, `symphonia/s28c-wave-pcm`, `symphonia/s28c-aiff-pcm`, `symphonia/s28c-native-flac`, `symphonia/s28c-mpeg-layer-1`, `symphonia/s28c-mpeg-layer-2`, `symphonia/s28c-mpeg-layer-3` |
| Legacy retained evidence | `symphonia/generated-pcm-wav` |

`descriptor_lists_exact_s28_foundation_fixture_ids` сравнивает полный set, чтобы
новая или потерянная row требовала осознанного обновления inventory. Fixtures
остаются hermetic: required tests не читают `test-assets`, не скачивают media и
не запускают внешний encoder/parser.

## Coverage inventory

`coverage/policy.json` классифицирует `demux-api`, `symphonia-demux` и
`web-media-http` как blocking crates. S28G guardrail fail-closed проверяет exact
classification и не позволяет одновременно записать owner в informational
group. `coverage/baseline.json` и `coverage/exceptions.json` в S28G не меняются.

### Known coverage limitation

`scripts/coverage.sh check` сейчас ожидаемо останавливается на известном
Session 28 relocation/ratchet blocker-е: default cargo-llvm-cov filename filter
по-разному считает перенесённые inline и отдельные test files. Это не regression
S28G и не повод объявлять coverage PASS. Исправление classification semantics,
baseline migration и exceptions принадлежит отдельному policy package.

## Out of scope

- HLS/DASH/ISM manifests, adaptive fetch и live/DVR;
- repeated init, discontinuity и refresh;
- новые codec/container families;
- random seek через dynamic Matroska `CodecState` boundary;
- удаление thin `DualStreamDemuxer`;
- patch removal, dependency bump или coverage baseline rewrite.
