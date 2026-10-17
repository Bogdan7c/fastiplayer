# Web-версии звука в меню «♪» (AC-3/E-AC-3 пакет F, 2026-10-16)

Решение владельца: у web-видео с несколькими версиями звука (AAC 2.0 и Dolby E-AC-3 5.1/AC-3) — подписать форматы и дать выбор в кнопке аудиодорожек. Default-выбор не менялся (пакет E: `DefaultAudioSelectionTier`, Dolby fallback-only, `mem:media-services/ac3-eac3-web-candidates`).

## Владельцы и границы
- Каталог/активная строка — `media-source-open` (`WebMediaStreamConfiguration::component_variant_projection`, `resolve_component_variant_action`). app-egui только проецирует.
- `app-egui/src/state/audio_tracks/`:
  - `web_versions.rs` — `web_audio_versions(parent_generation, &projection) -> Option<Vec<WebAudioVersionEntry{action, label, is_active}>>`: только независимая audio axis (`VideoAndAudio`/`AudioOnly`) с ≥2 строками; Coupled/VideoOnly/Unavailable/1 строка → `None`. Подпись `<Language> (<Codec> <layout>)`, без языка «Audio version N (…)», совпавшие подписи + «, N kbps». `channel_layout_label`: 1 Mono, 2 2.0, 6 5.1, 8 7.1, иначе «N ch». Язык — `labels::language_display_name` (общий со встроенными), кодек — `ui::url_sidebar::codec_label` (стал `pub(crate)`).
  - `menu_view.rs` — `AudioTrackMenuTarget::{EmbeddedTrack(TrackId), WebAudioVersion(ComponentVariantSelectionAction)}`, поле `AudioTrackMenuItem.target` (вместо `id`). `AudioTrackMenuView::compose(snapshot, Option<versions>)`: есть версии → строки = версии (встроенные дорожки web-источника НЕ показываются: одна отметка «играет», круг `B` не ломается); нет → как раньше. `from_snapshot` теперь `#[cfg(test)]`.
  - `web_version_switch.rs` — `AppState::audio_track_menu_view` (единый снимок на кадр, им пользуются `ui_runtime` и `B`), `request_web_audio_version_by_user` (поле `AppState.requested_web_audio_version`, последний выигрывает), `apply_requested_web_audio_version(playlist_runtime, renderer)` — зовёт `frame_prepare` после egui pass (рядом с URL sidebar action), pure `start_notice`/`terminal_failure_notice`.
- UI: `AudioTrackControlAction::SwitchWebAudioVersion(action)` (имя не `Select*` — clippy enum_variant_names), `menu.rs::control_action_for(target)`.
- `state/same_item_candidate_switch.rs`: `SameItemSwitchKind::Component { action, origin: ComponentSwitchOrigin::{UrlSidebar, AudioTrackMenu} }`, `apply_audio_track_menu_component_action` (тот же start path и sidebar safe error, что `apply_url_sidebar_action`), `poll_same_item_switch` читает origin до poll и на `SameItemSwitchAppPoll::Failed` для AudioTrackMenu вызывает `report_web_audio_version_switch_failed` (toast). `ActiveMediaSource::web_media_stream_configuration` → `pub(super)`.
- `media-source-open/src/web_media_stream_model/component_variants/equivalent_video.rs`: `replace_audio_keeping_visible_video` — Audio-ветка `replace`. Если точная `replace_audio` даёт `IncompatibleComponentPair` (Topology с sparse edges: Apple authoring — отдельная audio group на кодек и свой `EXT-X-STREAM-INF` на group с тем же видео), берётся ПЕРВЫЙ video row с той же видимой картинкой (точный codec + ширина/высота/fps/HDR, битрейт игнорируется) и совместимый с запрошенным звуком. Нет такого → прежняя ошибка (качество видео ради звука молча не меняется). Касается и строк audio в URL sidebar.

## Тексты (English)
Старт — «Audio: <label>» (`TransientReplaceKey::AudioTrack`), без подписи «Switching audio version»; NoChange — молча; Busy — «Another switch is already in progress»; Stale/устаревший action — «The audio version list is outdated; select the version again»; `ReplacementFailed`/прочее/SourceUnavailable — «Could not switch audio version»; Cancelled — «Audio version switch canceled». Sidebar inline error по-прежнему ставится.

## Память выбора
Выбор web-версии НЕ пишется в `track-memory.json` (решение владельца открыто): s07 `RememberedTrackHint` (язык/название/codec_id/ordinal) описывает встроенную дорожку; восстановление web-версии при открытии потребовало бы component reopen на старте (startup preference) — отдельная cross-layer задача. Session-only `WebMediaRememberedPreference` (Slice G) component switch тоже не трогает.

## Ограничения
- Coupled-only master (звук только muxed в variant-ах, разные кодеки в разных variant-ах) — версии в меню не появляются (это выбор качества/варианта, панель sidebar).
- yt-dlp audio-only formats идут через unified picker (`StreamFacet`), не через меню «♪».
- Atmos/JOC не подписывается: presentation не несёт флага object-based (`CHANNELS="16/JOC"` сводится к bed-каналам в `merge_audio_evidence`); нужен новый field в `WebMediaAudioComponentVariantPresentation`.
- Несколько встроенных дорожек внутри одной web-версии в меню не выбираются.

## Тесты
- app-egui `state/audio_tracks/web_versions_tests.rs` (AAC 2.0+E-AC-3 5.1 подписи и отметка активной, `B` по кругу через 3 версии, встроенные скрыты при версиях, embedded-only без изменений для Unavailable/Coupled/VideoOnly/1 строки, fallback/битрейт-подписи, раскладки, start/terminal notices); `state/same_item_candidate_switch/lifecycle_bridge/tests/audio_menu.rs` (production bridge: Paused+позиция сохраняются, pending selection = Component(action), отказ до barrier-а оставляет playback и даёт `Failed(SourceUnavailable)`).
- media-source-open `web_media_stream_model/component_variants_equivalent_video_tests.rs` (эквивалентная картинка выигрывает у раньше стоящего 1080p; без неё — typed `IncompatibleComponentPair`); сквозной `native_startup/hls/audio_versions_vertical_tests.rs` (loopback master с group `aac`/`ec3` → catalog с AAC активной → action на E-AC-3 → semantic reopen `RestoreOrBeginning(200 ms)` → `PositionedAt` → `A_EAC3` 6ch 48k → PCM RMS>0.01; старый action → Stale*). Фикстуры `crates/media-source-open/tests/fixtures/hls-audio-versions/` (generate.sh, README).
- Харнесс `ac3_vertical_tests.rs` (LoopbackOrigin, decode_to_pcm, …) — помощники `pub(super)` для соседних вертикалей.

## Живая приёмка D6 (2026-10-17)
- Loopback `python3 -m http.server`, master как в вертикальном тесте (group `aac` 440 Гц / `ec3` 880 Гц), 10×2 с сегментов: меню «♪» (кнопка в правой части панели) показывает «English (AAC 2.0)» / «English (E-AC-3 5.1)», по умолчанию AAC; `B` (uinput KEY_B) AAC→E-AC-3→AAC→E-AC-3 — каждый раз верный тон, пауза 120–140 мс на reopen, позиция сохраняется.
- Ловушка (1) ниже ЗАКРЫТА 2026-10-17 (допуск в один сегмент + удаление несовместимой версии, `mem:media-services/ac3-eac3-web-candidates` «Временная шкала»).
- Ловушки: (1) каталог HLS требовал точного выравнивания (`catalog/discovery.rs::alignment_for`: равная сумма EXTINF И равное число сегментов) — ffmpeg-овский `-f hls` даёт AAC 11 сегментов / E-AC-3 2.016+1.984 → нет compatibility edge → `IncompatibleComponentPair` → ВСЁ открытие падает с неточным «manifest metadata conflicts with probed media»; (2) переключение в последнем сегменте (18 с из 20): reopen отказывает `staged demux anchor lies after authoritative target` или ждёт `WaitingForVideoTargetFrame` до конца. Обе — общие для HLS, в бэклоге `user/ac3-eac3-support/README.md`.

## Проверки
`cargo test --locked -p app-egui --no-default-features`; `cargo test --locked -p media-source-open`; `cargo clippy --locked -p app-egui -p media-source-open --all-targets -- -D warnings`; `cargo fmt --all --check`; `python3 scripts/check-refactor-guardrails.py`; `python3 scripts/check_s42_guardrails.py`; `python3 scripts/s42_module_size_guardrail.py` (baseline `frame_prepare.rs` 1435).

Связано: `mem:subtitles/audio-tracks-memory-s07`, `mem:app-egui/same-item-candidate-switch-s25-2026-07-22`, `mem:media-source-open/core`.
