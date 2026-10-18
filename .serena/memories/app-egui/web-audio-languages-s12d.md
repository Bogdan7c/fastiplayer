# Языки звука (дубляж) yt-dlp в меню «♪» (сессия 12, этап 12d)

Решение владельца: по умолчанию оригинал (как YouTube, `language_preference`), другие языки выбираются в «♪» и `B`; переоткрытие на той же позиции через same-item switch.

## Владельцы
- service-ytdlp: `YtDlpNormalizedCandidate::declared_language_preference()`, `YtDlpCandidateSnapshot::inventory_video_of_compound(&selection)` (video-only строка inventory для compound `requested_formats` — так открывается YouTube по умолчанию; поиск по равенству request material).
- media-source-open `web_media_open/audio_languages.rs` (pub mod): `ExtractorAudioLanguageCatalog` (в `ExtractorMediaSourceState.audio_languages`), нейтрально `WebAudioLanguageChoice{action,language,is_original,is_active}`, `WebAudioLanguageAction` (поколение + индекс, `for_test` под test-fixtures), `WebAudioLanguageResolution::{NoChange,Stale,Ready(YtDlpCandidateOpenIntent)}`; `ExtractorMediaSourceState::{audio_language_choices, audio_language_switch_intent}`.
- Правила: лучший по fallback-рангу audio-only на язык, принимаемый `compose_inventory_av` с активным видео; оригинал = строго наибольший `language_preference` (ничья/нет данных — не помечать); <2 языков или muxed — пусто. Ранжирование общее с каталогом: `catalog::ranked_playable_audio`.
- catalog.rs `adopt_active_separate_pair`: после смены языка установленная пара заменяет строку того же видео в каталоге (иначе open падал «active Installed choice absent playable parent catalog»).
- app-egui: `media_open/web/audio_language_actions.rs` (`web_audio_language_choices`, `audio_language_switch_request`), `state/same_item_candidate_switch/audio_language.rs` (`SameItemSelectionRequest`, `apply_audio_language_action`), `SameItemSwitchKind::AudioLanguage` (origin меню «♪» для toast), `state/audio_tracks/web_audio_languages.rs` (подписи «Russian», «English (original)»), `AudioTrackMenuTarget::WebAudioLanguage`, `WebAudioMenuSource::{Versions,Languages}` (версии HLS/DASH приоритетнее), `AudioTrackControlAction::SwitchWebAudioLanguage`, `AppState.requested_web_audio_switch: Option<WebAudioSwitchRequest>`.
- Тексты отказов переиспользуют `web_version_switch.rs`.

## Тесты
- media-source-open `web_media_open/audio_languages/tests.rs`: fake yt-dlp + loopback + `prepare_yt_dlp_web_media`, вертикаль 440 Гц (оригинал) → действие ru → 880 Гц. Фикстуры `tests/fixtures/yt-dlp-audio-languages/` (generate.sh). Harness `web_subtitle_fetch::tests::{origin,yt_dlp_script}` стал `pub(crate)`.
- service-ytdlp `candidate/composition_tests.rs`; app-egui `audio_tracks/web_audio_languages_tests.rs`, `lifecycle_bridge/tests/audio_menu.rs` (AudioLanguage), widget-тест клика по языку.

## Память языка «для этого видео» (follow-up 12d)
- playlist-state: `RememberedWebAudioLanguage` (`web_audio_language.rs`), поле `TrackMemoryEntry.web_audio_language`, `TrackMemory::remember_web_audio_language`. Файл: необязательное поле записи `web_audio_language` (пишется только когда выбрано). Схема НЕ повышена (остаётся 2): v2 из 12c ещё не выпущена; v1 и v2 без поля читаются. Минус: dev-бинарник 12c с `deny_unknown_fields` счёл бы файл с полем повреждённым.
- Запись: только при `Installed` смены языка (`SameItemSwitchKind::AudioLanguage{purpose: UserChoice{remember_as}}` -> `SameItemSwitchCompletionOwner::remember_web_audio_language` -> `AppState::remember_user_web_audio_language`, ключ из активного источника). Отказ/устаревшее не пишут. Оригинал пишется тоже (перекрывает прежний дубляж).
- Восстановление: `schedule_remembered_web_audio_language` (`audio_tracks/web_audio_language_memory.rs`), хук одной строкой в `strong_media_open/pending/resume.rs` только на пути НОВОГО открытия (после return same-lineage; wiring guard в `track_memory/tests/wiring_guards.rs`). Запрос `WebAudioSwitchRequest::RestoreRemembered{action, attempts_left}` ждёт завершения открытия (Busy/Stale/нет идентичности, бюджет 300 кадров), любой другой исход снимает навсегда; `AudioLanguagePurpose::MemoryRestore` не пишет память и не даёт toast. Почему после установки: языки известны только из снимка форматов при открытии; цена — один лишний reopen. Suspend/resume и настройки сохраняют язык через `installed_reopen_intent`, поэтому не переключают повторно.
- Ограничение: смена качества на другое видео-разрешение (picker) открывает пару с аудио по умолчанию и язык не переносит (same-lineage путь не восстанавливает).

## Ограничения
- Выбор языка не пишется в track-memory (следующий шаг). Позиция сохраняется общим same-item путём, в media-source-open не проверяется.
- Язык без тега в `AudioTrackDescriptor` в список не попадает.
