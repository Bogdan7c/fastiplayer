# Session 00C1: staged media transaction и atomic install commit (2026-07-13)

Дополняет `mem:investigations/playlist-session-00c-candidate-video-resources-2026-07-13` и `mem:player-core/core`.

## Итог
Session 00C1 завершена PASS без playlist/controller types, D52 intent race, service classification, production callsite migration или начала 00D.

## Новые boundaries
- Public `PlayerWorker`/`PlayerCommandSender`: `stage_prepared_media_install`, `authorize_install_commit`, `cancel_media_install`.
- `MediaInstallVideoResourcePort` переносит exact Session 00C detached resource port в player worker, не раскрывая WGPU/materializer pointers.
- `MediaInstallControlReceipt` отделяет transport enqueue от owner outcome. Queue Full/Disconnected остаются `PlayerWorkerSendError`; исчезнувший sender после принятой команды — fatal `MediaInstallControlReceiptError::MissingOwnerOutcome`. После `AuthorizationAccepted` `MediaInstallReceipt::take_required_installed_after_authorization` типизирует missing/mismatched/unexpected terminal как fatal `AcceptedMediaInstallTerminalError`.
- Compatibility load остаётся destructive auto-authorize path и перед запуском supersede-ит staged candidate; это не strong transaction.

## Ownership/state machine
- `PlayerSession::staged_media_install` содержит максимум один `StagedMediaInstall` и один last-terminal tombstone.
- Candidate владеет `PreparedMedia`, pure audio/video plans, preallocated `MediaInstanceId`, configured detached `StartedVideoBackend` и resource port до authorize/cancel.
- Pure planning, resource acquisition, reply/backend matching, detached configure и status publication завершаются до `ReadyToCommit`; old Playing/Paused session/pipeline не мутируется.
- Matching authorization выполняет infallible `commit_staged_media` в одном worker owner turn, затем `MediaInstallProtocol` lossless публикует `Installed` до возврата/следующей команды.
- Stop/shutdown cancel active staged request до legacy lifecycle command. Stale/duplicate controls typed-reject-ятся.

## Atomic commit/release
- Commit извлекает old media owners через `PlaybackPipeline::retire_media_resource_owners`, устанавливает новый demux/media, already-configured decoder, tracks, instance, render generation, clocks и state, и только после switch освобождает old owners.
- Outstanding old-generation render lease удерживает old decoder в `PlaybackPipeline::retired_video_decoders`; late unsubmitted release идёт old decoder-у, submitted release — frame provider-у. Decoder owner удаляется после последнего lease. Новый decoder не получает old handle.
- Post-commit runtime error принадлежит new `MediaInstanceId`; hidden rollback запрещён.

## App half
`prepare_post_installed_commit` возвращает `PostInstalledVideoPipelineInvariantViolation` для missing/mismatched request/generation/unconfigured app half после player `Installed`. Это fatal invariant; successful token commit остаётся infallible pointer replacement.

## Fallible map
`MediaInstallFailureStage::ALL` содержит 11 stages, включая:
- `CandidateVideoResourceAcquisition`
- `CandidateVideoBackendMatching`
- `CandidateVideoBackendConfiguration`
- `CandidateVideoStatusPublication`

## Проверки
- `player-core`: 503 tests PASS.
- `video-backend-api`: 13 PASS.
- `render-wgpu-video`: 99 PASS.
- `app-egui`: 268 PASS.
- strict Clippy player/app, fmt, locked workspace check, refactor guardrails, git diff check и Serena diagnostics PASS.

Следующая разрешённая сессия — 00D. Production coordinator/callsites и D52 exact-instance intent race не начинать в рамках 00C1.
