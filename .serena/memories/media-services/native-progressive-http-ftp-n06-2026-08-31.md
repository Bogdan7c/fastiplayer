# N06 — Native progressive HTTP(S)/FTP(S) (2026-08-31)

## Итог

- Direct HTTP Ogg/WebM и FTP/FTPS Ogg теперь классифицируются и открываются нативно при `yt_dlp.enabled=false`.
- N07 не начинался. Локальный commit N06 должен называться `feat(web-media): open direct HTTP and FTP media natively`.
- Контекст foundation: `mem:core`, `mem:media-services/progressive-http-s22-2026-07-22`, `mem:media-services/progressive-ftp-s37-2026-07-25`, `mem:media-services/ytdlp-scoped-cookies-prefetch-ogg-2026-08-09`.

## Ownership и boundaries

- `service-direct-media` теперь только classification/locator owner. Он парсит absolute HTTP(S)/FTP(S), строит checked `TransportRequestTarget`, хранит exact secret-bearing reopen/persistence identity и публикует только redacted `Debug`/`Display`/`safe_label`.
- Service больше не владеет transport registry, concrete HTTP provider, prefetch, demux runtime/config или progressive worker. `src/transport.rs` удалён; normal dependencies ограничены `demux-api`, `source-core`, `thiserror`, `url`, `web-media-transport-api`. Это закреплено `scripts/check-refactor-guardrails.py`.
- `app-egui::direct_progressive_open` — composition root. Он регистрирует existing `web-media-http` и `web-media-ftp`, строит protocol-specific `TransportOpenRequest`, передаёт ровно один returned `TransportInput` в production `WebDemuxComposition` и только для streaming input оборачивает demuxer в `ProgressiveDemuxer`.
- Locator parsing и app runtime ownership не смешаны: classification не выполняет network I/O; startup/queue preparation владеют cancellation и prepared-media lifecycle.

## Capability contract

- `DemuxRegistry::supports_extension(extension, input_capability)` проверяет реальные factory registrations, не выбирая factory и не заменяя content probe.
- Direct classification принимает extension только если production registry объявляет его и для `SeekableBytes`, и для `StreamingBytes`. Старого MP4/MOV/MKV/WebM allowlist больше нет; Ogg/Opus следуют Symphonia registration автоматически.
- Extension берётся только из URL path; query/fragment не участвуют. Manifest и unsupported protocol сохраняют разные typed classification errors.

## Data-plane и security invariants

- HTTP Range сохраняет seekable/prefetch semantics existing `web-media-http`. HTTP `200` сохраняет forward-only semantics; initial response body передаётся demux worker-у без второго GET/downloader/probe.
- FTP использует только `TransportOpenRequest::for_ftp`; конструкция API не позволяет приложить HTTP cookie/header/redirect material. Credentials, path, query и fragment не попадают в safe projections/errors.
- Direct runtime не зависит от `service-ytdlp` и не создаёт subprocess. Functional fixtures держат `yt_dlp.enabled=false` и process spy остаётся 0 после open/seek/reopen.
- Явный reopen создаёт новый transport cohort; seek работает внутри существующего demux/source lifecycle и не переклассифицирует root URL.

## Functional evidence

- HTTP Range Ogg/Vorbis проходит production transport + Symphonia + audio decoder до nonzero PCM: exact 3 requests на первый open, buffered nonzero seek не увеличивает count, explicit reopen доводит exact count до 6.
- HTTP full-body `200` Ogg проходит до nonzero PCM, остаётся non-seekable и выполняет ровно 1 request.
- Credentialed FTP Ogg проходит production FTP provider до nonzero PCM; REST-backed seek и reopen увеличивают RETR accounting, safe projections не раскрывают secrets, process spy = 0.
- HTTP WebM проходит production Symphonia demux + FFmpeg VP9 decode + HostPlanar materialization + offscreen WGPU renderer submit/readback; readback nonzero, decoder-owned resource released.
- URL adapter test фиксирует HTTP Ogg/WebM и FTP Ogg как native direct при `yt_dlp.enabled=false`.

## Verification

- `cargo fmt --all -- --check`
- `git diff --check`
- `python3 scripts/check-refactor-guardrails.py`
- `cargo test -p demux-api --locked extension_capability_follows_registered_input_shapes`
- `cargo test -p service-direct-media --locked`
- `cargo test -p app-egui --locked direct_progressive -- --nocapture` — 5/5.
- `cargo test -p app-egui --no-default-features --locked direct_progressive -- --nocapture` — 4/4.
- `cargo clippy -p demux-api -p service-direct-media -p app-egui --all-targets --all-features --locked -- -D warnings`
- `cargo clippy -p app-egui --no-default-features --all-targets --locked -- -D warnings`
- `cargo check --workspace --all-targets --all-features --locked`
- Serena diagnostics пусты для production boundary files и новых focused tests. На parent `content_probe_tests.rs` rust-analyzer по-прежнему показывает известный false-positive для external `#[path]` audio fixture; Cargo/Clippy собирают этот модуль успешно.

## Known limitations

- WebM renderer vertical gated существующей default `ffmpeg` feature и требует доступный WGPU Vulkan adapter; no-default matrix намеренно проверяет только native Ogg HTTP/FTP path.
- Classification требует explicit path extension; content-type-only/no-extension URLs не стали direct в N06.
