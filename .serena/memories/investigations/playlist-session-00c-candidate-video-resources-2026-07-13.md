# Playlist Session 00C candidate video resources (2026-07-13)

Status: PASS. Session 00C завершена без player staged media transaction, production callsite migration, playlist types или D52. Полный handoff: `user/playlist_queue_implementation_plan.md`, section `Handoff Session 00C`.

## Neutral backend boundary

`video-backend-api::detached_backend` владеет explicit typestate:
- `DetachedVideoBackend::from_started` принимает только что запущенный backend, который ещё не active.
- consuming `configure_stream` возвращает `ConfiguredDetachedVideoBackend` только для `Configured|Unchanged`; `AbsentDecoder`, unexpected `Cleared`, `Unsupported`, `Backpressure`, `Fatal` остаются distinct typed failures и освобождают backend exactly once.
- только `ConfiguredDetachedVideoBackend::into_started_backend` возвращает installable artifact будущему 00C1 commit.
- `DetachedVideoBackendResourcePort` generic по request ID и задаёт fake-able request/reply/status/cancel handoff без WGPU/app/player-core deps.
- resource failures distinct: `AdmissionBackpressure`, `Unavailable`, `ResourceExhausted`, `StartupFailed`.

## App renderer owner

`crates/app-egui/src/video_pipeline_candidate.rs` владеет ровно одним `StagedVideoPipelineCandidateSlot`: request ID, exact non-zero `RendererGeneration`, backend/materializer descriptor, materializer и submission binding. Отдельный `resource_driver.rs` сохраняет concrete creation только в composition root:
- VA-API factory -> existing WGPU submission wrapper -> DMA-BUF materializer.
- FFmpeg factory -> same submission wrapper -> HostPlanar materializer.
- driver никогда не получает active pointers, поэтому candidate preparation failure не может мутировать active pair.
- второй admission отвергается до driver startup; backend pool/second PlayerSession/hidden retry отсутствуют.

Slot принимает matching player status, хранит один lossless terminal outcome и distinct accounting. Pre-barrier requested/superseded/stale-renderer/suspend/disconnect cancellation освобождает player half через port и app half через owned drop exactly once. Status/request mismatch не очищает current candidate.

Matching Installed сначала проходит request/generation/config validation и создаёт `PreparedPostInstalledVideoPipelineCommit`. Его `commit` infallible: только заменяет prepared app pointers/binding и публикует Installed outcome; startup, allocation, device poll/wait и callback drain отсутствуют. Token удерживает exclusive slot borrow. Если token defensive-drop-нут, app half восстанавливается в `PostInstalledCommitRequired`; pre-barrier cancel после этого запрещён, и lifecycle обязан повторно взять token и закончить exact commit.

Existing `WgpuSubmissionQueueBinding`/submitted release CAS ownership не менялось. Candidate creation/cancel не rebind-ит active queue; old submitted callbacks продолжают release exactly once.

## Verification

- `cargo test -p video-backend-api`: 13 PASS.
- `cargo test -p app-egui`: 267 PASS, включая 11 Session 00C scenarios.
- `cargo test -p render-wgpu-video`: 99 PASS.
- targeted strict Clippy, fmt, locked workspace check, refactor guardrails, git diff check, Serena references/diagnostics: PASS.
- Context7 pinned WGPU 29.0.3/winit 0.30.13 verified callback/poll and suspended-surface lifecycle assumptions.

Session 00C1 выполнена; актуальный ownership/worker/commit handoff находится в `mem:investigations/playlist-session-00c1-staged-media-transaction-2026-07-13`. Next: Session 00D only. Concrete factories/render types по-прежнему не переносить в player-core/video-backend-api.