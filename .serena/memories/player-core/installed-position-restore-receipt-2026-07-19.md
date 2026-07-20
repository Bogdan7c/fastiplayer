# Terminal receipt восстановления позиции (2026-07-19)

## Корневая причина
- `InstalledMediaStateRestoreOutcome::Applied` раньше публиковался сразу после принятия `PlayerCommand::Seek`, хотя video seek ещё находился в `SeekLanding`/`Scrubbing` и не достиг authoritative commit.
- Startup, доверяя этому false terminal, отправлял `StartPaused`; Pause отменял активный SeekLanding. Demux/decoder поколения расходились, а последующий Play мог кормить decoder зависимыми кадрами без корректно завершённого seek bootstrap. H.264 лишь особенно явно проявлял повреждение как `Invalid frame_num`; дефект был общим lifecycle race, а не особенностью кодека или контейнера.

## Owner boundary и инварианты
- `PlayerSession` владеет `PendingInstalledPositionRestore`, потому что уже владеет seek generation, commit, cancel, timeout, fatal error и media identity.
- Pending receipt коррелируется одновременно по `MediaInstallRequestId`, `MediaInstanceId` и seek generation. `Applied` публикуется только из общего final seek commit. Смена instance возвращает `StaleInstance`; supersede/cancel/timeout/fatal закрывают receipt typed `Failed(Position)` с исходной `PlayerError`.
- `session/seek_receipts.rs` является общей внутренней точкой settle для exact timeline seek и installed-position restore. Публичный `player-core` API и enum outcomes не менялись.
- `KeepStart` остаётся синхронным `Applied`, потому что seek lifecycle не запускается. Ошибки, опубликованные синхронным стартом seek, извлекаются только из событий текущего dispatch и не подменяются generic `PositionUnavailable`.
- Решение codec/container/backend-neutral: в нём нет проверок H.264/H.265/VP9/AV1, MKV/MP4/WebM, VA-API/FFmpeg или packetization. Любой backend проходит один и тот же seek terminal contract.

## Проверки
- Focused tests: `crates/player-core/src/session/tests/installed_media_restore.rs` закрепляет ожидание commit, cancel без false Applied, передачу fatal demux error и StaleInstance при замене media.
- Реальный regression fixture H.264/MKV после restart дошёл до final seek commit, затем Pause; последующий Play работал без `Invalid frame_num` flood. Workspace all-features tests на Rust 1.96 и strict Clippy прошли.

Связанные memories: `mem:player-core/core`, `mem:playlist/resume-position-sidecar-2026-07-19`, `mem:app-egui/startup-orchestration-s17`.


## S13 playback-window уточнение (2026-07-20)
- `InstalledPositionRestore::SeekTo` всегда принимает публичную relative позицию активного window.
- Pending receipt не завершается на demux seek: relative target переводится в absolute source time, а `Applied` публикуется только после matching seek commit, как и для media без window.
