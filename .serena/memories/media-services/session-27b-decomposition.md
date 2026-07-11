# Session 27B demux/media services decomposition (2026-07-11)

- Census зафиксирован в `user/session_27b_demux_media_services_census_and_backlog_2026-07-11.md` вместе с отдельными follow-up prompts.
- `service-youtube/src/http_stream.rs` теперь единолично владеет playback-only blocking HTTP transport: reqwest client/header adaptation, response-body read loop, streaming writer EOF/error handoff и fetcher thread startup.
- `service-youtube/src/lib.rs` остаётся media-open orchestration facade; `process.rs` сохраняет yt-dlp executable/selector/timeout/stdout/stderr/termination policy; `resolver.rs` сохраняет metadata normalization, candidate construction и selected identity refresh; `selection.rs` сохраняет stream/HDR capability rules.
- Runtime behavior не менялся: source cancellation/Range/prefetch ownership остаются в source-core/media-prefetch/http_refresh; YouTube process и candidate policy не изменены.
- `symphonia-demux` census подтвердил существующие owners: packet_mapper, seek_mapper, track_mapper, matroska_metadata, byte_source, streaming_source. Остаточный construction/pre-scan split вынесен в отдельный backlog prompt, чтобы не смешивать чувствительный cursor/reset contract с текущей правкой.
- Проверки: focused tests для symphonia-demux, service-direct-media, service-youtube, media-core, source-core, media-prefetch прошли; app-egui startup_media 16 tests прошли. Asset-dependent tests остались ignored и требуют explicit local paths по Session 02.
