# S30 FLV/F4F demux (2026-07-23)

Читайте вместе с `mem:core`, `mem:codec-core/vp-flv-foundation-s30`, `mem:audio/core`, `mem:demux-api/core` и `mem:media-services/core`.

## Владение и границы

- Новый first-party crate `flv-demux` владеет raw FLV header/tag framing, strict PreviousTagSize/StreamID/filter validation, transactional replay всех bytes неуспешного framing candidate-а перед bounded resync, independent per-track u32-millisecond unwrap, sequence/config lifecycle, bounded AMF0 `onMetaData`, sparse actual keyframe index, transactional VOD seek, bounded recovery и F4F adapter.
- Neutral `demux-api` не изменён. Raw FLV зарегистрирован только для `SeekableBytes | StreamingBytes`; F4F — только для `OrderedSegments`. `FlvDemuxFactory` имеет exact container IDs `flv` и `f4f`; extension `f4v` намеренно отсутствует.
- F4F adapter принимает только готовые `OrderedSegmentKind::Media` fragments с доказанной bounded ISO topology: ровно по одному `afra`, `abst`, `moof`, `mdat`, причём `afra` обязан быть первым. `afra`, `abst`/`asrt`/`afrt` и `moof`/`traf`/`tfhd`/`trun` валидируются по declared counts, flags и размерам; из непустого `mdat` извлекаются headerless FLV tags с validated PreviousTagSize. Standalone initialization/bootstrap намеренно отклоняется: F4M/HDS bootstrap, network и RTMP state находятся вне crate. Ordered sequence exact, discontinuity требует fresh config и decoder reset event.
- App composition вынесена в `app-egui/src/web_media_demux_registry.rs`: web registry и planner snapshot строятся из одних descriptor-ов Symphonia + FLV/F4F без factory-wide capability leakage. Accidental MPEG-TS web registration/hint отсутствует; существующий local S29 MPEG-TS path не менялся.
- `web-media-core` теперь нормализует `f4v` как `ContainerFamily::IsoBmff`; `flv` и `f4f` остаются отдельными families.

## Codec/config lifecycle

- Legacy: H.264/AVC, AAC, MP3, platform-endian PCM U8 only, little-endian PCM U8/S16LE и exact `A_ADPCM_SWF`. AAC принимает только packet types 0/1; legacy AVC не принимает Enhanced-only type 3; reserved/command video frame types и header-only legacy audio fail typed до decoder/state mutation. Platform-endian PCM16, G.711 и прочие legacy codec IDs fail typed.
- Enhanced single-track FourCC: `vp08`, `vp09`, `av01`, `avc1`, `hvc1`. VP config и keyframe probe, AVC/HVC config+probe и AV1 config+probe переиспользуют `codec-core`. H.264/H.265 packets остаются length-prefixed; signed SI24 CTS сохраняется. VVC, legacy HEVC-12, multitrack, ModEx, MPEG2TS и unknown packet types fail typed.
- Identical sequence config — no-op. Changed config атомарно заменяет TrackInfo и ставит ровно один `TracksChanged` до зависимого packet. Malformed replacement не стирает last valid config. SequenceEnd не EOF, но запрещает следующий packet до SequenceStart.
- После framing loss/discontinuity output закрыт до fresh sequence config и доказанного codec-core video keyframe. До config packets безопасно пропускаются, после video config пропускаются до proven keyframe; весь reacquisition расходует единый `recovery_bytes` budget, который не сбрасывается повторной ошибкой внутри того же gate и исчерпывается отдельной typed `RecoveryGateBudgetExhausted`, не смешанной с framing `RecoveryExhausted`. Audio-only path открывается после fresh exact audio config/header.

## Metadata, seek и bounds

- AMF0 parser принимает bounded object/ECMA/strict-array/string/number subset, удерживает duration/title и capped `keyframes.times/filepositions`; все f64→Duration/u64 conversions fallible, NaN/inf/overflow metadata игнорируются без panic, anchors считаются недоверенными.
- Seek сканирует actual tags в named tag bound, подтверждает codec config и actual codec-proven keyframe, затем коммитит anchor вместе с независимыми video/audio u32-unwrapper states. Anchor packet после seek сохраняет post-rollover epoch. Исчерпание scan budget без EOS/covering anchor возвращает typed `SeekScanBudgetExhausted`; failure/cancellation восстанавливают source cursor и parser/timestamp/config/event state, rollback failure не игнорируется.
- Все tag, metadata, recovery, index, seek-scan, fragment и box limits представлены `FlvDemuxOptions`/`FlvLimit`; `fragment_boxes` расходуется как единый budget на всё вложенное ISO box tree, а не отдельно на каждый container. Real media fixtures не требуются.

## Проверки

- `cargo test -p flv-demux` — 31 hermetic tests: progressive, short-read/live shape, config no-op/change/malformed/end, enhanced mappings/rejections, strict legacy wire validation, codecs, timestamp rollover + rollover seek, bounded AMF conversions/index, VOD seek/rollback/budget exhaustion, transactional framing replay и bounded recovery/cancel. F4F topology/sequence/discontinuity fixtures живут отдельно в `crates/flv-demux/src/tests/f4f_tests.rs`.
- `cargo test -p web-media-core`
- focused `app-egui` demux registry/capability tests
- strict Clippy для touched crates, workspace check, refactor guardrails, fmt и diff-check.
