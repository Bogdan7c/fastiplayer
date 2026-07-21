# Config schema v7: global preferred video height (2026-07-21)

- `CURRENT_SCHEMA_VERSION = 7`; поддерживаемые legacy versions — v2-v6. v2-v5 по-прежнему мигрируют `[youtube]` -> `[yt_dlp]`, v6 получает новый default без startup rewrite.
- `rustiplayer_config::YtDlpConfig::preferred_video_height: Option<PreferredVideoHeight>` — единственный durable global quality knob. `None` означает обычный BestPlayable. Config-owned newtype принимает только `1..=16_384`; TOML сериализует его scalar-числом. Config crate не зависит от `web-media-core`.
- Settings registry для `YtDlpConfig` hand-written в `crates/config/src/schema/yt_dlp_settings.rs`, потому что поле nullable/newtype. Stable setting id: `yt_dlp.preferred_video_height`; config-owned static choices: BestPlayable и 144/240/360/480/720/1080/1440/2160/4320p. Валидное custom TOML значение сохраняется как unavailable current, а не теряется.
- `app-egui::web_media_quality` — единственная composition boundary: config newtype -> `web_media_core::PreferredHeightPolicy`. Compile-time assert держит config и neutral max bounds синхронизированными.
- `service_ytdlp::select_yt_dlp_stream` принимает neutral `PreferredHeightPolicy`. Порядок: playable HDR bucket policy -> configured codec order -> exact height -> closest lower -> closest higher -> missing height -> прежние quality score / stream-id tie-breaks. Invalid candidate height — typed `InvalidVideoHeight`, не silent missing.
- Settings application contract: `MediaService / MediaSourceLifecycle / MediaSourceRebuild`. Apply preferred height reselect-ит только active YtDlp source через existing strong reopen/Installed+restore path; direct/local не rebuild-ятся от quality-only change. Mixed network+quality route всё равно rebuild-ит remote source. Rollback использует тот же owner path.
- Manual per-item override не имеет persisted config key: current v7 strict TOML отвергает `item_video_height_override`; exact selected stream остаётся runtime active-source state.
- Golden fixture: `crates/config/tests/fixtures/current_schema_v7.toml`. Playback smoke self-test ожидает schema v7.
- Focused coverage: config migration/roundtrip/bounds/settings accessor; None mapping; height fallback/HDR/codec/invalid-candidate selection; Settings apply/persist/reopen and runtime-only override absence.

Related: `mem:config/schema-v6-ytdlp-migration-2026-07-17`, `mem:settings-ui/application-contract-s08`, `mem:media-services/ytdlp-hdr-selection-s16`.
