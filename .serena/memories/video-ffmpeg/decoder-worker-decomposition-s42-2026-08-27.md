# S42 Wave C1: FFmpeg decoder worker decomposition (2026-08-27)

## Архитектура

- Behavior-neutral relocation: playback-facing parent `crates/video-ffmpeg/src/decoder_thread.rs` сохраняет `FfmpegVideoDecoderThread`, neutral `VideoDecoderThreadHandle` implementation, bounded packet/control/frame/error/shutdown channels, worker spawn construction, completion/control-pressure state и `FfmpegWorkerLifecycle`.
- Новый private production child `crates/video-ffmpeg/src/decoder_thread/worker.rs` владеет только `FfmpegDecoderWorker` owner-loop: shutdown/control selection, configure/clear, seek flush, EOF continuation, packet dispatch, decoded-frame publication/resource rollback и fatal reporting.
- `FfmpegDecoderWorker` state definition и literal construction остаются в parent, поэтому FFmpeg codec/frame/packet owners по-прежнему создаются только внутри spawned owner thread. Единственный новый seam — private `pub(super) FfmpegDecoderWorker::run`; public, crate-wide и neutral API не расширялись.
- Existing `decoder_thread/{send_receive,lifecycle,host_resources,stream_config,color_metadata}.rs` не менялись. `send_receive` остаётся владельцем низкоуровневого libavcodec send/receive state machine.

## Сохранённые инварианты

- Durable packet completion записывается ровно один раз только после `progress_report.packet_completed`; activity notification не стала accounting source.
- EAGAIN/EOF и receive order не менялись: pending packet идёт раньше EOF continuation и нового select; accepted NULL drain повторяет receive-side continuation, но не отправляет EOF повторно.
- Configure/Clear/seek Flush очищают pending packet и pending EOF generation; Flush отдельно дропает только queued pre-seek packets.
- Frame publication rollback освобождает resource ровно один раз при Full/Disconnected; normal resource lifetime завершается только через `release_frame`.
- Shutdown имеет отдельный bounded signal; disconnected frontend остаётся terminal даже при full pool и queued packet. Fatal error transport остаётся sticky bounded channel.
- Relocated impl сравнён с HEAD byte-for-byte после удаления единственного doc-comment и `pub(super)` visibility token.

## Размеры и проверки

- Production lines: parent `781`, new worker child `428`; existing production children: send_receive `720`, host_resources `613`, lifecycle `58`, stream_config `84`, color_metadata `94`. Parent и все production children <=800; S42 baseline не менялся.
- Focused decoder thread: `cargo +1.96.0 test -p video-ffmpeg --features ffmpeg --locked decoder_thread::tests` — 29/29 PASS.
- Full crate: all-features 88/88 PASS; no-default 60/60 PASS; raw FFI boundary PASS.
- Real generated H.264/MPEG-TS AUD-005 regression PASS: accepted=16, delivered completions=16, terminal in-flight=0, no-output packets=14; он проходит production demux → worker decode/frame release → EOF → exactly-once completion.
- Strict Clippy all-features/all-targets и no-default/all-targets с `-D warnings` PASS; rustfmt, diff check и refactor guardrails PASS.
- Exact S42 acceptance `service-ytdlp --test final_acceptance_s42` — 24/24 PASS.
- Global `scripts/check_s42_guardrails.py` ожидаемо RED только на shared stale legacy module-size inventory; для `decoder_thread.rs` теперь reported stale baseline after reduction, а не oversized category C.
- Независимый reviewer rerun с тем же generated H.264/MPEG-TS прошёл full WGPU vertical: `AUD013_FIXED before_generation=1 before_pts_us=0 after_generation=2 after_pts_us=2000000 submit=completed release=completed`.
- Эта WGPU vertical остаётся `#[ignore]` и non-hermetic runtime acceptance: ей нужен доступный Vulkan adapter. Adapter мог быть software Vulkan, поэтому PASS доказывает production demux → FFmpeg decode → HostPlanar materialization → WGPU submit/completion → exactly-once release до и после seek, но не доказывает использование physical GPU.

Связанные контракты: `mem:video-ffmpeg/software-design`, `mem:video-ffmpeg/durable-packet-completion-aud005-2026-08-23`, `mem:video-ffmpeg/bounded-worker-shutdown-aud015-2026-08-23`.
