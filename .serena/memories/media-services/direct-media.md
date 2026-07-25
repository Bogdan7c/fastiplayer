## S31 shared adaptive HTTP note (2026-07-23)

- `source-core` получил generic `fetch_bounded_single_hop` для metadata/media full GET и exact Range с manual redirects; его использует новый `web-media-adaptive`. Existing direct-media S22 open/classification/prefetch flow не менялся и не маршрутизируется через adaptive owner. Полный boundary: `mem:media-services/adaptive-transport-s31-2026-07-23`.

# Direct Media Service

> **Superseded notice (2026-07-03):** любые упоминания hover preview, hover predecode, hover budget/reservation, timeline-hover prepare или hover overlay ниже являются историческими и не описывают активный контракт. Актуальные owners и запреты: `mem:core` и `mem:frame-server/core`. Остальная non-hover информация этой memory остаётся действующей.

- `service-direct-media` is the generic direct media URL opener. It must stay neutral: no `player-core`, no UI, no renderer/backend, no YouTube candidate/capability semantics.
- v1 URL policy: only absolute `http(s)` URLs whose URL path has explicit `.mp4`, `.mov`, `.mkv`, or `.webm` extension. Query/fragment do not count for extension detection. IP URLs with supported extension are allowed; IP/hostname URLs without supported extension are typed unsupported. `.mov` is treated as a QuickTime/ISO BMFF extension hint and still relies on Symphonia probing for real container/track support. `rtsp`, HLS `.m3u8`, DASH `.mpd`, unsupported extensions, missing host, and parse failures are typed startup/open errors. No `HEAD`, Content-Type mapping, or byte sniffing in v1.
- Session 10B secret-safe contract: `parse_direct_media_url` creates typed `DirectMediaUrl`, validates policy through `url` but preserves the original direct identity exact (including functional/signed query). `Debug`/`Display`/`safe_label` are redacted; raw identity exists only behind `expose_secret_for_open` / `expose_secret_for_persistence`. See `mem:media-services/secret-safe-locators-s10b`.
- Open flow: typed `DirectMediaUrl` -> `SourceRuntimeConfig::from_network_config` -> `source_core::SecretHttpUrl` -> `HttpRangeSource::open` -> require `source_core::Seekability::Seekable` -> wrap in `media_prefetch::PrefetchingByteSource` using network prefetch fields -> `SymphoniaDemuxer::from_byte_source_with_options(extension_hint, safe_label, demuxer_options)` -> `DirectMediaOpenResult` with demuxer/tracks/duration/seekability/safe source label.
- Error taxonomy lives in `DirectMediaOpenError`: invalid/unsupported URL, source config, prefetch config, typed `PrefetchStartup` (redacted typed locator context + original `media_prefetch::PrefetchStartupError` source chain), demux config, Range/source error, non-seekable source, demux/probe error. Invalid/unsupported protocol, manifest and extension reasons are fixed typed markers: errors never reflect raw input/path payload, and safe host labels are bounded. The service owns error adaptation only; `media-prefetch` retains worker spawn/shutdown/join ownership.
- `app-egui::startup_media` owns CLI routing: YouTube hosts via `service_youtube::is_supported_youtube_url()` go to capability-aware YouTube startup; other `http(s)` or authority-style URLs go through `service-direct-media`; non-URL arguments remain local file paths. `app-egui` converts `DirectMediaOpenResult` to `PreparedMedia::from_external_label(...)` before sending it to `PlayerWorker`. Since S27, app-owned timeline hover network source opening also uses `service-direct-media` directly for direct HTTP seekable Range sources; non-seekable direct sources degrade to typed hover unsupported/open-failed outcomes and must not reset playback.
- Focused coverage: `cargo test -p service-direct-media` covers URL policy, non-Range rejection, and a structured in-memory WAV over local Range 206 through Symphonia; `cargo test -p app-egui` covers CLI routing; `cargo test -p service-youtube` covers YouTube host allowlist plus structured WebM Range/fallback/live transport. Real direct HTTP Range media is explicit local manual acceptance via `scripts/media-regression.sh --scenario direct-http-range --path <file>` (see `mem:testing/media-fixtures`).

## Session 10C prepared-envelope adapter
- `DirectMediaOpenResult::media_metadata()` exposes a read-only `MediaMetadata` snapshot before `into_demuxer`; this lets app build the reusable descriptor without a second direct open.
- Direct service still returns no `PreparedMedia`, knows no playlist/player policy, preserves exact typed `DirectMediaUrl`, and keeps all formatting redacted. Coordinator details: `mem:app-egui/media-open-coordinator-s10c`.

## S15A routing clarification (2026-07-20)

- `service-direct-media` policy не расширялась: только HTTP(S) с explicit supported media extension. FTP(S)/RTMP никогда не передаются direct opener-у.
- App-owned единый URL registry сохраняет register order direct-media → yt-dlp. Успешная direct classification фиксирует `MediaOpenSourceRequest::Direct`; последующая open failure не возвращается в registry и не вызывает yt-dlp retry.
- Extended yt-dlp input schemes gated отдельно registered `Implemented` provider capability; production S15A list содержит exact `Ftp`/`Ftps` после S37, RTMP пуст до S39. Это не transport feature `service-direct-media`.


## S22 progressive HTTP migration (2026-07-22)

- Classification, `DirectMediaUrl`, safe labels and locator/error redaction remain service-owned and unchanged.
- Open adapter now uses `web-media-http` only through S21T `TransportRegistry`, then neutral `DemuxRegistry` with `SymphoniaDemuxFactory`; see `mem:media-services/progressive-http-s22-2026-07-22`.
- Range responses retain existing seekable + media-prefetch behavior. Non-Range `200` responses become forward-only progressive demux instead of `NonSeekable` rejection.
- MP4/MOV, MKV and WebM supply both extension and real container hints. Adapter constructs all registries/demuxers before returning `DirectMediaOpenResult`, so failures remain before player mutation.
