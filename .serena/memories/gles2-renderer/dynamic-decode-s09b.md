# Сессия 09b: режим декода Dynamic (2026-10-25, DONE)

Отчёт: `user/gles2-renderer/results/09b.md`, решения владельца 43–46 — README плана.

## Что это
`video.preferred_backend = "dynamic"` (config schema v25): на старте файла как `auto` (HW, если играбелен). Если HW-декодер устойчиво не успевает, этот файл один раз переходит на SW на лету (прежний `install_video_pipeline_plan`: заморозка кадра + точный seek), info-toast. Следующий файл снова с HW. По умолчанию остаётся `auto`.

## Владельцы и границы
- Детектор (player-core, нейтральный): `session/decoder_pace.rs` `DecoderPaceMonitor` (чистая логика на инжектированном времени) + `decoder_pace/tick_bridge.rs` (`PlayerSession::observe_decoder_pace`, вызывается в `tick/mod.rs` после `process_pending_video_packets`). Событие `PlayerEvent::VideoDecoderFallingBehind(DecoderPaceEvidence)`. Пороги `DecoderPaceThresholds` в `PlayerTickConfig.decoder_pace`: тишина 2 с, окно 4 с, порог 10 % пропущенных.
- Пропущенные = late-drop (точный счётчик) + голодание очереди показа по вине декодера (вход есть в начале И в конце шага, выход не заблокирован потребителем) в кадрах. Ожидаемые = Σ(wall_dt × rate)/кадровый интервал — любая скорость (решение 44).
- Не считается: не Playing, активный seek, остановленные аудио-часы (`session/audio_stall.rs` `PlayerSession::audio_clock_stall`, им же пользуется планировщик), late-drop в тике с обрезанным `MAX_OBSERVATION_GAP` (250 мс).
- Одно событие на прогон (медиа, render_generation, скорость): `reported_run`; seek/смена декодера/скорости/файла перевзводит.
- Политика (app-egui): `mem:app-egui/dynamic-decode-policy`.
- Требование играющего видео (бэклог п. 12 закрыт): `state/video_stream_requirement.rs` `ActiveVideoStreamRequirement`, приходит с кандидатом `video-backend-api::DetachedVideoBackendSelection::stream_requirement`, активируется в `state/strong_media_open/candidate_commit.rs`, Stop → None.
- Предупреждение GLES2 в настройках: `mem:settings-ui/hardware-decode-warning-b6-2026-10-10`.
- Новый вид ошибки рендера `PlayerRenderErrorKind::VideoBackendSwitchFailed` → `PlayerErrorKind::RuntimeError` (типы ошибок рендера вынесены в `player-core/src/worker/render_error.rs`).

## Замер (T410, GLES2)
- Переход через 6,4–6,6 с на 1080p60/1080p30/720p60; 720p30 остаётся на HW. Доля пропусков: здоровый HW ≤ 0,008 (780M ≤ 0,001), «не тянет» 1,17–1,43, граница 1,5× на 720p30 — 0,30. Порог 10 % с запасом.
- После перехода SW 60 fps ≈ 51 новый кадр/с из 60 (так же у чистого `software`; 30 fps — полные 30), детектор 0 % — потери в загрузке/показе GLES2, решение 46: в сессии 11/12a.
- Логи для анализаторов: debug "Video decoder pace window closed without falling behind", info "Video decoder is falling behind playback", info "Dynamic decode switched to software" (поля expected/late/starved/ratio/rate). RUST_LOG `player_core::session::decoder_pace=debug`.

## Грабли
- Бинарь до 09b читает только schema 24: конфиг v25 он заменяет умолчаниями в памяти — для сравнения со старым бинарём нужен отдельный конфиг v24.
- `playlist.load_siblings = true` по умолчанию: в приёмке соседи по папке уходят в очередь (так «нашёлся» мнимый п. 15 бэклога).
