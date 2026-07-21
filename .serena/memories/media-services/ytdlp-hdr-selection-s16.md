# yt-dlp HDR selection — актуальный контракт (2026-07-17)

- Config schema v7 хранит `yt_dlp.hdr_selection` как `YtDlpHdrSelection::{SdrOnly, PreferHdrWhenAvailable}` со stable ids `sdr_only` / `prefer_hdr`, default `SdrOnly`. Legacy schema v2-v5 `[youtube]` мигрируется в `[yt_dlp]`, v6 получает default preferred height; подробности в `mem:config/schema-v7-quality-preference-2026-07-21`.
- Metadata/settings owner: `rustiplayer-config::YtDlpConfig`; `rustiplayer-settings` относит `yt_dlp.enabled`, `yt_dlp.hdr_selection`, `yt_dlp.resolve_timeout_ms` к `MediaService` route.
- Selection owner: `crates/service-ytdlp/src/selection.rs`. App startup выполняет resolve candidates -> `select_yt_dlp_stream` -> open exact selected stream id; capability policy не переносится в resolver/process.
- `YtDlpStreamCandidate` хранит opaque stream id, redacted descriptors, codec/profile/bit-depth/chroma/color requirement, quality score и `YtDlpDynamicRange::{Sdr, Hdr, Unknown}`. Dynamic range берётся только из typed yt-dlp metadata; description/quality labels не являются policy input.
- Кандидат считается SDR/HDR только при согласии manifest dynamic range с `VideoDecodeRequirement.color`. Unknown/противоречие даёт typed `YtDlpCandidateRejectionReason::UnknownDynamicRange`, после чего selection продолжает поиск.
- `SdrOnly` допускает только SDR. `PreferHdrWhenAvailable` сначала проверяет HDR через полный `SystemCapabilities` intersection (decoder + frame contract + renderer + HDR-to-SDR), затем автоматически откатывается к SDR; strict HDR-required режима нет. Global preferred height применяется только внутри уже выбранного HDR/codec bucket-а: exact -> lower -> higher, поэтому не ослабляет HDR или codec policy.
- Bare `vp9` нормализуется в VP9 Profile 0 / 8-bit / 4:2:0 с `VideoColorMetadata::sdr_bt709_limited()`. Bare `vp9` с HDR hint остаётся insufficient; подробный VP9 Profile 2 сохраняет typed HDR metadata requirements.
- Focused tests: `crates/service-ytdlp/src/selection.rs`, resolver tests, `crates/config/src/store/tests.rs`, `crates/app-egui/src/startup_media.rs`.

## S21C follow-up (2026-07-21)
- Neutral selection policy вынесена в `web-media-playback-plan`: HDR/codec/container tie-breaks применяются только после capability filtering и S20Q preferred-height bucket.
- Exact stale semantics теперь проверяют exact и semantic identity через `ExactSelectionIdentity`; детали: `mem:media-services/web-playback-planner-s21c-2026-07-21`.
