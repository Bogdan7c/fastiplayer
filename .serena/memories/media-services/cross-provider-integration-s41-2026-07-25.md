# S41 cross-provider integration (2026-07-25)

## Итог

- S41 закрывает единый production path `candidate -> transport/demux -> PreparedMedia -> strong install` без нового provider-specific runtime и без изменения public player/provider API.
- Canonical S00 `profile.json` по-прежнему хранит compatibility targets со статусом `Target`; runtime disposition вынесен в отдельный machine-readable `crates/service-ytdlp/compatibility/2026.07.04/runtime-coverage-s41.json`.
- Manifest one-to-one покрывает все 13 S00 target rows: 12 exact rows имеют `Implemented`, aggregate `rtmp-family-flv` имеет `ProfileExcluded` и остаётся S39 identity-only/no-wire evidence. S36 ISM live, S38 HDS live и S40 special expansions не получают fake playback rows.

## Общий boundary

- `app-egui::web_media_open::prepare_yt_dlp_web_media` остаётся единым selection/planner/provider entry.
- `WebOpenRuntime::open_candidate` владеет concrete dispatch: Smooth, HDS, HLS, DASH, иначе progressive HTTP/FTP; каждый branch возвращает один neutral `PreparedYtDlpWebMedia`.
- Новый internal intent-boundary `app-egui::media_open::prepare_yt_dlp_player_media` принимает demuxer и named `YtDlpPreparedMediaAttachments { timeline_port, demux_seek_port, playback_window }`. Он единственный задаёт порядок attachments: worker-receipted seek -> static playback window -> dynamic live timeline. Static window + live timeline fail-closed до Ready/authorization.
- Normal coordinator preparation, startup orchestration и settings active-source rebuild теперь используют этот один helper. Ранее startup/settings вручную дублировали порядок attachments, что создавало риск расхождения provider semantics.
- `AppState::install_prepared_media_strong`, coordinator Ready/authorize/Enqueued/Installed semantics, player ownership и post-installed restore не менялись.

## Coverage и тесты

- `crates/service-ytdlp/tests/cross_provider_integration_s41.rs` проверяет exact S00<->S41 row set, owner sessions, отсутствие `Planned`, 12 Implemented rows, единственный RTMP exclusion, общий production path и существование каждого declared focused evidence symbol.
- `crates/app-egui/src/media_open/preparation.rs` focused tests доказывают: live port устанавливается до barrier; static seek+window проходят единым helper-ом; static window + live timeline отклоняются pre-barrier.
- Manifest traceability охватывает BestPlayable/Exact/global height/runtime override, separate A/V, semantic refresh, group part, CUE, Playing/Paused/live restore, local/direct, auth, pre/post barrier, restore/settings/shutdown.
- Provider suites остаются у owners: progressive HTTP, FTP, HLS VOD/live, DASH VOD/live, Smooth VOD и HDS VOD. RTMP/special playback tests запрещены до новой exact approved row/fixture.

## Проверки

- S41 focused: 3 PASS.
- Implemented provider package matrix: PASS (service-ytdlp, web-media-http/ftp/hls/dash/smooth/hds; service process tests выполнялись serially из-за известного transient ETXTBSY race при parallel temp executable launch).
- `app-egui --no-default-features`: 881 PASS.
- `player-core`: 611 PASS.
- Strict touched Clippy, Rust 1.96 locked workspace check, rustfmt, diff check, refactor guardrails и Serena diagnostics PASS. Новый standalone Cargo integration test показывает только ожидаемый rust-analyzer `unlinked-file` hint; Cargo test target успешно compiled/run.

См. также `mem:media-services/core`, `mem:app-egui/media-open-coordinator-s10c`, `mem:player-core/core`.