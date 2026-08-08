# Exact video requirement preflight (2026-07-17)

Связано с `mem:player-core/core` и `mem:investigations/playlist-session-00c1-staged-media-transaction-2026-07-13`.

## Причина изменения

Strong media install раньше разрешал отложить неполный H.264/H.265/VP9 requirement до первого packet-а. Из такого plan-а получался `DetachedVideoBackendSelection` без exact backend ID. Production app правильно отказывался самостоятельно угадывать backend, поэтому atomic transaction сохраняла прежнее media, а Next/Previous внешне выглядели как no-op. Второй дефект позволял после packet refinement оставить reselection pending, но в следующем tick отправить тот же packet старому decoder-у.

## Инварианты

- `codec-core::video_requirement_evidence_policy(VideoCodec)` — исчерпывающий registry источников codec evidence. Добавление нового `VideoCodec` обязано компиляционно выбрать policy; wildcard запрещён.
- Exact strong install завершает requirement до resource request: container metadata -> codec-private -> при разрешённой policy buffered packet probe. Только после этого capability layer выбирает exact backend и frame contract.
- `DetachedVideoBackendSelection` всегда содержит `String` backend ID. Состояния `unprobed`/optional ID больше нет; app только материализует уже выбранную player-ом пару backend/transfer contract.
- Compatibility ingress, у которого нет detached resource port, имеет отдельный `StagedVideoBackendPlan::CompatibilityDeferred` и никогда не вызывает production resource port.
- `PreparedMedia` владеет всеми событиями, прочитанными preflight-ом. `PrefetchedDemuxer` после commit-а воспроизводит их FIFO перед продолжением inner demuxer-а; packet/audio/lifecycle/EOF не теряются. Seek очищает replay как события старой позиции.
- Staged packet probe ограничен именованными budget-ами: 512 events и 64 MiB encoded payload. TracksChanged во время preflight, EOF без пригодного header-а, demux error и budget overflow дают typed install failure до мутации active media.
- Runtime packet refinement возвращает `ActiveVideoRequirementRefinement::{DecoderReady, BackendReselectionRequested}`. Пока reselection pending, первый packet остаётся в FIFO и не может попасть в старый decoder ни в текущем, ни в следующем tick.
- AV1 exact preflight использует полный container snapshot либо валидный `av1C`. AV1 без этих данных fail-closed с typed error: packet-level AV1 requirement parser пока отсутствует и его нельзя заменять предположением 8-bit SDR.

## Расположение

- Registry: `crates/codec-core/src/requirement_preflight.rs`.
- Strong planning/probe: `crates/player-core/src/session/staged_video_preflight.rs`.
- Replay owner: `crates/player-core/src/media_opening.rs` и `media_opening/prefetched_demuxer.rs`.
- Exact detached API: `crates/video-backend-api/src/detached_backend.rs`.
- Runtime FIFO gate: `crates/player-core/src/session/tick/video_decoder_io.rs`.

## Focused regression tests

- `vp9_hdr_packet_preflight_selects_p010_and_replays_interleaved_audio`.
- `incomplete_av1_never_requests_unprobed_backend_resource`.
- `packet_refinement_waits_for_backend_reselection_without_old_decoder_send`.
- codec-core policy/AV1/H.265/VP8 tests в `requirement_preflight.rs`.

Проверки 2026-07-17: full `scripts/ci-checks.sh tests`, strict Clippy для codec-core/video-backend-api/player-core/app-egui, fmt, diff-check и refactor guardrails — PASS.


## S21W resumable packet probe (2026-07-21)

- Staged video preflight теперь player-owned continuation: `StagedMediaPreparation::Pending(PendingStagedPreflight)` сохраняет `PreparedMedia`, audio plan, `StagedVideoPlanner`/reader budget+uncertainty progress, protocol/resource port, retry и timeout deadlines.
- Pre-install fence состоит из exact `MediaInstallRequestId + StagedPreflightGeneration`; `MediaInstanceId` создаётся только после готового video plan и playback-window preparation.
- `DemuxReadEvent::TemporarilyUnavailable` возвращает `Pending(DemuxRetryHint)`, не добавляется в `PreparedMedia` replay queue и не расходует packet/event/byte probe budgets. Следующий worker tick после deadline продолжает тот же planner, не начинает probe заново.
- `MediaInstallFailureStage::VideoPreflightTimeout` terminal-resolve-ит wall-clock expiry. Supersede/cancel/shutdown сохраняют прежние typed causes и exactly-once completion; detached backend до успешного preflight не запрашивается.
- Early authorization остаётся `MediaInstallControlOutcome::NotReady`; Ready→Authorize→Installed commit barrier и exact response ordering не изменены.

## Exact backend constraint lookup boundary (2026-08-05)

- `capability-core::SystemCapabilities::find_playable_video_output_for_backend` владеет поиском playable output по exact `DecodeBackendId` и `VideoDecodeRequirement`; player-core не читает внутренний `playable_video_outputs` для этого решения.
- Staged preflight сначала вызывает полный `check_video_requirement` и сохраняет прежний порядок HDR/frame/render validation, а только затем применяет request-scoped exact-backend constraint.
- Focused capability tests закрепляют exact-backend hit, miss на другом backend-е и miss при несовместимом requirement; вертикальные software-policy tests player-core подтверждают выбор FFmpeg и отсутствие hardware request.
