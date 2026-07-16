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
