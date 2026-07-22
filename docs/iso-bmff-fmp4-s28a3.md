# S28A3: fragmented ISO BMFF completion

## Профиль и authoritative sources

S28A3 завершает только finite VOD fragmented MP4/CMAF baseline. Единственным
parser owner остаётся локальный `symphonia-format-isomp4` 0.6.0 patch; второй ISO
parser не добавлен.

Перед реализацией source сверялся с официальным crates.io archive
`symphonia-format-isomp4-0.6.0` SHA-256
`2d179a01305b3505940135a9f0180d6ef4b487912748fe97554756f120fbd05e` и с
[W3C ISO BMFF Byte Stream Format](https://www.w3.org/TR/mse-byte-stream-format-isobmff/).
Для media segment применён minimum contract W3C: `moof` содержит `traf`, каждый
`traf` содержит `tfdt`, а все referenced samples обязаны полностью находиться в
`mdat`. Random-access landing использует effective ISO sample flags.

## Владение и инварианты

- `atoms/tfdt.rs` парсит version 0/1 `baseMediaDecodeTime`; unsupported version,
  truncated value, missing и duplicate `tfdt` завершаются structural error-ом.
- `TrafAtom` сохраняет exact `tfdt` и проверяет empty-duration/trun cardinality.
- `MoofSegment` начинает track timeline с exact `tfdt`, поэтому non-zero start и
  gaps не схлопываются в сумму предыдущих fragment durations.
- Packet read order остаётся container/DTS order. `trun` version 0/1 composition
  offsets формируют отдельный PTS, включая signed offset и non-monotonic PTS.
- Effective sample flags имеют приоритет: per-sample `trun`, затем
  `first_sample_flags`, затем `tfhd.default_sample_flags`, затем
  `trex.default_sample_flags`. Video-handler seek откатывается к ближайшему
  доказанному sync sample не позднее target; reserved/не доказанные flags не
  считаются sync. Audio и другие non-video handlers выбирают timestamp sample,
  потому что video decode anchor им не требуется.
- Classic `stss` seek path не менялся.
- Fragmented zero `mvhd`/`mdhd` без `mehd` или доказанного `sidx` публикует
  `duration=None`, а не ложный ноль. Явный `mehd`, non-zero header duration и
  существующий `sidx` остаются источниками duration.
- Clean EOF распознаётся только на box boundary. Частичный header, box payload,
  `moof`/`traf`/`trun`, declared `mdat` и sample payload возвращают structural
  parse error. Defensive generic `symphonia-demux` fallback для чужого
  `IoError(UnexpectedEof)` не менялся.

## Hermetic proof

- Direct patch suite покрывает `tfdt` v0/v1/non-zero/malformed, missing/duplicate,
  composition offsets, effective flags, sync rollback, first-sample flag semantics,
  handler-specific seek policy, clean EOF, declared-box truncation и обрыв skipped
  box на source без известной длины.
- `factory::tests::fragmented_isomp4` переиспользует generated FFmpeg 8.1 AAC
  fMP4 corpus из S28A2 и проверяет exact DTS gap, seek rollback, unknown/mehd
  duration, clean EOF и truncation на `moof`/`traf`/`trun`/`mdat`/sample payload.
- Полный `symphonia-demux` suite сохраняет codec-private mapping, classic MP4
  metadata/color/orientation/PCM и defensive EOF behavior.

## Осознанные ограничения

- Ordered inputs остаются non-seekable; fMP4 seek доказывается на seekable
  local/Range byte source.
- Repeated init, discontinuity, live/DVR, manifests HLS/DASH/ISM и segment refresh
  принадлежат S31+.
- `avc3` и новые codec/sample-entry variants не входят в S28A3.
- Fragment duration не вычисляется из будущих live fragments и не берётся из
  manifest-а; без container authority она остаётся неизвестной.
