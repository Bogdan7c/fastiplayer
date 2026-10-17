# AC-3 / E-AC-3 web/adaptive кандидаты (2026-10-16, пакет E)

## Что сделано
- `web-media-core`: `CodecFamily::{Ac3, Eac3}` (две точные family, audio). Токены `parse_codec_kind`: `ac-3`|`ac3`|`a_ac3` → Ac3; `ec-3`|`ec+3`(JOC)|`eac3`|`a_eac3` → Eac3. Без wildcard: `ac-4`, `A_AC3/BSID9`, `A_TRUEHD`, `dtsc` → Unknown. Тесты `normalized.rs` вынесены в `normalized/tests.rs` (лимит 800 строк).
- Маппинг в `AudioDecodeCodecFamily::{Ac3,Eac3}` во всех трёх местах: `web-media-playback-plan::candidate::audio_codec`, `media-source-open::web_media_open::catalog_capabilities::audio_family`, `service-ytdlp::candidate::planning::audio_requirement`. Capability intersection как у остальных: нет family в snapshot → typed `AudioUnavailable` (planner) / `HlsCatalogCapabilityRejection::Unsupported` / `HlsCatalogBuildError::ProviderDefaultRejected{CapabilityRejected}`. Соседняя Dolby family не доказывает другую.
- DASH: `dash-mpd-core` принимает `codecs="ac-3"/"ec-3"` только в ISO-BMFF (WebM → `UnsupportedMediaEvidence`); `DashAudioChannelConfiguration::DolbyChannelMask(DashDolbyChannelMask)` для схем `tag:dolby.com,2014:dash:audio_channel_configuration:2011` и `urn:dolby:dash:audio_channel_configuration:2011` (модуль `src/dolby_channel_mask.rs`: ровно 4 hex, ненулевая; парные биты Lc/Rc, Lrs/Rrs, Lsd/Rsd, Lw/Rw, Vhl/Vhr, Lts/Rts считаются за 2 канала; F801=6, FA01=8, A000=2). Невалидная маска → `Unsupported` (как раньше: Representation отвергается локально, MPD не падает). `web-media-dash`: `catalog::channel_count` учитывает маску; `lane_proof::audio_codec_matches` — `Ac3↔A_AC3`, `Eac3↔A_EAC3` (каналы маски сверяются с demux).
- HLS: `hls-playlist-core::MediaRendition::{declares_object_based_audio, speaker_channel_count}` — для `CHANNELS="16/JOC"` (второй parameter ≠ `-`) число объектов не evidence раскладки. Используется ТОЛЬКО в `catalog/rows.rs::merge_audio_evidence` (сверка с доказанной дорожкой: 16/JOC против 5.1 больше не `ManifestEvidenceConflict`). Identity-сопоставление renditions (`open::rendition_matches`, `native_ingress::rendition_evidence`, `catalog/reopen.rs`) по-прежнему raw `channel_count` (тест `channel_count_evidence_matches_rfc_channels_primary_parameter`).
- UI: `app-egui::ui::url_sidebar::codec_label` → "AC-3"/"E-AC-3" (те же написания, что `state/audio_tracks/labels.rs`).

## Политика default-выбора (решение владельца ОТКРЫТО)
- Единственный владелец: `web_media_core::DefaultAudioSelectionTier::for_codec` (`src/audio_selection_tier.rs`): Ac3/Eac3 → `FallbackOnly`, всё прочее → `Primary` (`Primary > FallbackOnly`). Смысл: прежние default-решения не меняются от того, что Dolby-кандидаты стали воспроизводимыми; Dolby выбирается по умолчанию только без Primary-альтернативы. Явный выбор пользователя не затрагивается.
- Применение: HLS `native_ingress::compare_candidates` — после video codec rank, до высоты/bandwidth (variant-уровень `declared_audio_tier`); DASH native default (`catalog/default_selection.rs::best_default_audio` + coupled min_by) — внутри каждого шага; yt-dlp `AudioFallbackRank` — поле `codec_tier` сравнивается первым (`with_codec_selection_tier`), codec rank yt-dlp: Eac3=25, Ac3=20 (ниже mp3, как `acodec` order yt-dlp).
- Чтобы предпочитать многоканальный E-AC-3 — поменять `for_codec` (одна строка) и тесты `*keeps_primary*`/`fallback_only*`.

## Не сделано / ограничения
- Smooth Streaming: `smooth-streaming-manifest-core` профиль только H.264+AACL; init segment синтезируется из manifest без bitstream → `dac3/dec3` не из чего строить (нужен pre-read первого fragment-а + `ac3-syncframe`). yt-dlp Smooth candidates с не-AAC уже typed-отклоняются (`service-ytdlp/candidate/transport/smooth.rs`).
- HDS/FLV: в FLV SoundFormat нет AC-3; HDS catalog требует `A_AAC` — пропущено сознательно.
- HLS packed audio (`.ac3/.ec3` сегменты) не проверялся (зависит от raw AC-3 demux-а в web-registry).
- Прежний баг (не AC-3): native HLS master только из audio-only variant-ов падает в `NativeHlsSourceState::new` («component selection shape is not consistent with catalog layout»), воспроизводится и с AAC.
- (закрыто пакетом G) DASH и HLS fMP4 E-AC-3 runtime: `media-source-open/src/native_startup/fmp4_dolby_vertical_tests.rs` — фикстура `symphonia-demux/tests/fixtures/ac3/h264-eac3-5p1-fragmented.mp4` режется по top-level box-ам (init ftyp+moov, 2× moof+mdat); static MPD (muxed `avc1.42c00a,ec-3`, Dolby mask F801, SegmentList) и HLS master+`EXT-X-MAP` → production native admission → `A_EAC3` 48k 6ch (6 каналов из `dec3`, см. isomp4 patch) → 32 пакета → декодер → тон в каждой lane.
- Прямой HTTP/FTP сырой `.ac3/.eac3/.ec3` подключён (`ContainerFamily::Ac3Elementary`, `mem:ac3-elementary-demux/core`). yt-dlp кандидат с ext `ac3`/`eac3`/`ec3` теперь планируется как progressive raw AC-3 (раньше Unknown → отказ); default-tier Dolby = FallbackOnly не меняется.
- HLS packed audio (`.ac3/.ec3` сегменты без TS) по-прежнему не проверялся: `new_hls` регистрирует только Symphonia+TS, raw AC-3 туда не добавлен.

## Тесты
- `web-media-core`: `normalized::tests::dolby_digital_spellings_map_to_distinct_exact_families`, `audio_selection_tier::tests::*`.
- `web-media-playback-plan`: `tests::audio_containers::dolby_audio_rows_intersect_exact_snapshot_family`, `dolby_descriptor_and_runtime_family_must_agree`, `audio_rank::tests::fallback_only_codec_tier_yields_to_primary_before_channel_count`.
- `service-ytdlp`: `candidate::planning_ac3_tests::*` (JSON rows ac-3 6ch / eac3 2ch → family+каналы; AAC stereo > E-AC-3 5.1 > E-AC-3 2.0 > AC-3 2.0).
- `media-source-open`: `web_media_open::catalog_capabilities::tests::*`; сквозные `native_startup::hls::ac3_vertical_tests::*` — loopback HTTP → production native HLS admission → MPEG-TS demux (фикстуры `mpeg-ts-demux/tests/fixtures/ac3/`) → `A_EAC3` 5.1/44.1k (15 packets) и `A_AC3` mono → `audio::create_audio_decoder` → PCM (RMS>0.01, 15×1536×6 samples); snapshot без Eac3 → typed `ProviderDefaultRejected{CapabilityRejected}`.
- `dash-mpd-core`: `dolby_channel_mask::tests::*`, `tests/dolby_audio_profile.rs`; `web-media-dash`: `discovery::lane_proof::tests::dolby_manifest_contract_requires_exact_family_and_mask_channels`, `catalog::default_selection::tests::*`.
- `hls-playlist-core`: `tests/parser.rs::object_based_channels_are_not_speaker_count_evidence`; `web-media-hls`: `native_ingress::ac3_tests::*`, `catalog::rows::tests::*`.

## Проверки
`cargo test --locked -p web-media-core -p web-media-playback-plan -p service-ytdlp -p dash-mpd-core -p web-media-dash -p hls-playlist-core -p web-media-hls -p media-source-open`; `cargo clippy --locked -p <те же> -p app-egui --all-targets -- -D warnings`; `cargo check --workspace --all-targets --locked`; `cargo fmt --all --check`; `python3 scripts/check-refactor-guardrails.py`; `python3 scripts/check_s42_guardrails.py`.

Выбор версии пользователем (меню «♪», эквивалентный video row для audio group по кодекам): `mem:app-egui/web-audio-versions-menu`.

Связано: `mem:audio/ac3-eac3-decoder`, `mem:mpeg-ts-demux/core`, `mem:media-services/web-playback-planner-s21c-2026-07-21`, `mem:media-source-open/core`.
