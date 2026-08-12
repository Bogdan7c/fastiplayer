# Ручная web-media playlist acceptance (2026-08-04)

- Добавлены `user/web-media-playlist-acceptance.xspf` и `user/web-media-playlist-acceptance.md`.
- XSPF содержит 13 top-level public resources: отдельный rich YouTube URL-settings case плюс двенадцать крупных compatibility rows (progressive ISO-BMFF, progressive WebM, progressive audio, HLS TS VOD, HLS fMP4 VOD, HLS live/DVR candidate, DASH fMP4 VOD, DASH WebM VOD, DASH live/DVR, Smooth VOD, HDS VOD, FTP progressive).
- Порядок намеренно чередует transport owners и media layouts, чтобы ручной прогон проверял queue transitions, seek, EOF и stale-resource cleanup, а не только isolated open.
- Инструкция делает обязательной полную проверку единственной вкладки URL: secret-safe projection, status, dependent mode/codec/resolution/FPS/HDR selectors, Playing/Paused same-item switch, pending disable, active no-op, fallback/error и HLS/DASH variants.
- Все 13 active locators были read-only проверены exact stock binary `yt-dlp 2026.07.04` 2026-08-04. Public URL availability и format inventory остаются mutable; `SOURCE DRIFT`/unavailable не являются player PASS или автоматически player bug.
- Known limitation: BBC/Akamai HLS live закрывает exact HLS-live/DVR row только если текущий manifest реально предоставляет seekable DVR range. Live-only без DVR — `SOURCE DRIFT` для этой row.
- Known limitation: GNU fixture подтверждает exact FTP only. Direct `ftps://ftp.gnu.org/...` probe на pinned yt-dlp возвращает `Unsupported url scheme: "ftps"`; FTPS branch агрегированной row остаётся `NOT RUN`.
- Clean `--ignore-config --no-plugin-dirs` применяется только к preflight probe. Production app намеренно сохраняет system yt-dlp config/plugins; isolated XDG config root не отменяет system-wide config.
- Workflow связан из `docs/runtime-acceptance-manifest.md`, `docs/web-media-s42-final-acceptance.md` и `docs/web-media-compatibility-matrix.md`. Он не заменяет 29-case S42 topology/auth/privacy runner и не является automated acceptance.
- Verification: `xmllint --noout docs/web-media-playlist-acceptance.xspf`; exact track count 13; `cargo test -p playlist-io --test xspf_v1 --locked` (14 PASS); `cargo test -p app-egui --no-default-features --locked cli_route_classifies_each_supported_playlist_format_before_local_media_open` (1 PASS); `git diff --check`.
- Production Rust API, architecture boundaries, player/render logic and commands did not change; this update adds only manual acceptance artifacts/workflow.

Follow-up: runtime-поломка HLS TS VOD и production fix описаны в `mem:testing/hls-ts-vod-runtime-fix-2026-08-04`; исходное утверждение выше об отсутствии production-изменений относится только к созданию acceptance artifacts до этого follow-up.

Связанные memories: `mem:testing/playback-smoke`, `mem:testing/media-fixtures`, `mem:testing/hls-ts-vod-runtime-fix-2026-08-04`, `mem:app-egui/sidebar-controller`, `mem:app-egui/web-media-picker-slice-g-2026-07-26`, `mem:media-services/ytdlp-topology-summary-2026-08-04`.

Follow-up 2026-08-10: BBC/Akamai row сейчас имеет 6 `avc3` video variants, ~898.560 s sliding DVR и `mp4a.40.5` HE-AAC audio вне текущего AAC-LC profile. Production fix и real render proof: `mem:media-services/hls-live-avc3-2026-08-10`.

## DASH live/DVR row 06 production regression pass (2026-08-10)

- Row 06 uses `https://livesim.dashif.org/livesim/segtimeline_1/utc_httpxsdate/spd_6/tsbd_60/testpic_2s/Manifest.mpd` and now passes dynamic MPD admission, playback, repeated ordered refresh, DVR seek and expired-pause Play recovery.
- Real GUI proof used VA-API H.264 plus AAC: playback reached about 75 s, remained paused for more than 72 s (past the 60 s DVR depth), recovered to a fresh live target, and then continued for more than 80 s through multiple MPD/EOF continuation cycles.
- Final telemetry during that run: Playing, about 59 FPS, 0 visible frame drops, 0 surface drops, 0 audio underruns, healthy frame pacing and advancing MPRIS position. Repeated-frame accounting accumulated while paused and preroll seek-discard accounting is expected; neither is a decoded/presented frame-drop regression.
- Automated regression coverage includes parser/planner/runtime suites, a hermetic render-reaching live runtime test, no-old-DVR-head-refetch assertion, player retention-before/after-worker-receipt tests and true-authoritative-expiry cleanup. Focused DASH/adaptive tests, media/player tests, strict touched-package Clippy, workspace all-target check, diff check and refactor guardrails pass.
- Architecture handoff: `mem:media-services/dash-live-s35-2026-07-24` and `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`.


## HDS VOD row 09 production regression pass (2026-08-12)

- Row 09 uses `https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel.ism/.f4m` and now opens as the 12:14 HDS presentation and reaches real H.264/AAC playback/render.
- Three independent false-negative assumptions were fixed: delivered F4F media fragments contain `afra/moof/mdat` and normally omit the separately-owned `abst`; terminal zero-duration `afrt` END_OF_PRESENTATION may use fragment ID 0 outside media ordering; app HDS sniff deadline follows configured source read timeout instead of hidden 2 s.
- Real KWin proof on the production release binary reached Tears of Steel frames, 3312 packets, 879 decoded video frames and 799 `video_frames_presented`, with the 12:14 duration and advancing timeline. One separate existing telemetry issue remains: the overlay retained stale `VA-API VP9` text while the actual HDS content watermark and packet/config evidence were `avc1`/H.264.
- Regression fixtures now mirror the real provider/bootstrap boundary. Focused HDS/FLV/bootstrap/app tests, strict Clippy, refactor guardrails, diff-check and the full workspace test gate pass.
