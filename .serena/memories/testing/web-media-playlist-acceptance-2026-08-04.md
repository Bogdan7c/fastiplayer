# Ручная web-media playlist acceptance (2026-08-04)

- Добавлены `docs/web-media-playlist-acceptance.xspf` и `docs/web-media-playlist-acceptance.md`.
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

Связанные memories: `mem:testing/playback-smoke`, `mem:testing/media-fixtures`, `mem:app-egui/sidebar-controller`, `mem:app-egui/web-media-picker-slice-g-2026-07-26`, `mem:media-services/ytdlp-topology-summary-2026-08-04`.