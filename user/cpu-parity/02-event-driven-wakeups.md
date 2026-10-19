# Сессия 02: пробуждения по событию, а не по таймеру

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 02 плана `user/cpu-parity/`: убери холостые пробуждения каждые 2 мс у аппаратного декодер-потока и у player-worker в установившемся воспроизведении. Сначала прочитай `user/cpu-parity/README.md` и `user/cpu-parity/results/00.md`. Замер «до/после» обязателен; добавь счётчики пробуждений в сводку диагностики, чтобы эффект был виден в логе.

## Проблема

Живой прогон HW H.264 1080p60: `pauses=38182` за ~25 с (≈1500 холостых тиков worker-а в секунду), `wake_reason="decode_readiness"`, `wake_delay_ms=2.0`, present queue 8/8. Две петли подпитывают друг друга:

1. **Декодер-поток** (`crates/video-vaapi/src/decoder_thread/runtime_loop.rs:160-216`, `DECODER_FRAME_PUBLISH_RETRY_MS = 2` в `config.rs:46`): при полном канале кадров делает `control_rx.recv_timeout(2 мс)` и на каждом повторе в ветке `TrySendError::Full` (`runtime_loop/frame_publication.rs:190-205`) вызывает `send_frame_publish_pressure_event` → `notify_decoder_activity` → пульс worker-у. Сигнала «в канале кадров появилось место» нет: освобождение слота worker-ом декодер узнаёт только по таймауту или по `ReleaseZeroCopy`.
2. **Worker** (`crates/player-core/src/session/tick/wakeup.rs:185` `decoder_readiness_wakeup_plan`, `:564-640` `decoder_readiness_poll_needed_for_state`, `worker.rs:91` `DEFAULT_DECODER_READINESS_POLL_INTERVAL = 2 мс`): пока держит не отданный пакет при полной очереди, планирует 2-мс опрос; любой пульс из `decoder_pulse_receiver` (`worker/runtime_wait.rs:331-420`) — полный тик.

## Архитектура

- **Владелец «пульса»:** `video-core::VideoDecoderActivityNotifier` — семантика не меняется (epoch + `try_send` в канал ёмкости 1), меняются **поводы**: пульс только при реальном изменении состояния — опубликован кадр, принят пакет, ошибка, переход EOF drain, первое попадание в `Full` (переход в давление), а не каждый повтор. Диагностическое событие давления — только при изменении счётчиков (или с rate-limit), и оно не должно теряться на переходе.
- **Декодер ждёт «место в канале»:** владелец — `video-vaapi::decoder_thread` (`runtime_loop.rs`, `decoder_thread.rs:121` создаёт `frame_tx`). Варианты: (а) worker после `recv` из `frame_rx` шлёт лёгкий «room» сигнал по уже существующему пути (например, через control-канал или отдельный бounded(1) канал, добавленный в `select!`); (б) декодер при `Full` ждёт только `ReleaseZeroCopy`/control без таймаута, если известно, что освободить место может только worker/рендерер. Выбрать по коду: вариант должен сохранять немедленную реакцию на flush/seek/reconfigure/shutdown. Таймаут 2 мс оставить как страховку только для случаев, когда notifier недоступен, и сделать его явно «fallback».
- **Worker спит до дедлайна:** владелец — `player-core::session::tick::wakeup`. Когда декодер зажат по выходу (present queue ≥ target и пакет ждёт), а notifier доступен, план пробуждения — `FramePtsDeadline`/audio refill, не 2-мс опрос. Семантика `decoder_readiness_poll_needed` для seek-in-flight и недобора очереди сохраняется.
- **Наблюдаемость:** в сводку `Playback diagnostics summary` (`worker/runtime_publish.rs`) добавить за окно: число пробуждений worker-а по причинам, число тиков, число пульсов декодера; в декодер — число повторов публикации (`decoder_publish_retry_count` уже есть) и число «room»-пробуждений. Без этих счётчиков сессия не считается доказанной.

Memories: `mem:player-core/core`, `mem:video-vaapi/core`, `mem:player-core/command-delivery-reserve-ux17` (резерв очереди команд — не сломать), `mem:player-core/audio-starvation-buffering-resume-2026-08-27`.

## Инварианты

1. Новый кадр, ошибка, EOF drain, принятый пакет — всегда будят worker (lost-wakeup защита через epoch `activity_since` остаётся).
2. Flush/seek/reconfigure/release/shutdown прерывают любое ожидание декодера немедленно.
3. Латентность публикации кадра не растёт: кадр, для которого появилось место, уходит в канал не позже, чем сегодня (≤ 2 мс), а в идеале сразу.
4. Аудио-подпитка (high/low water) и PTS-дедлайны не зависят от этих изменений; worker без видео (аудиофайл) ведёт себя как раньше.
5. FFmpeg-путь (уже полностью блокирующий) не регрессирует; общий notifier-контракт один для обоих бэкендов.

## Тесты

- `video-vaapi` decoder_thread: fake-канал ёмкости 1, publish при `Full` → ровно один пульс и одно событие давления на вход в давление, ноль — на повторах; после «room» кадр публикуется без ожидания таймаута; flush во время ожидания — немедленный выход.
- `player-core` wakeup: при `present_queue == target`, pending packet и доступном notifier план = `FramePtsDeadline` (не `DecodeReadiness`); при недоступном notifier — прежний 2-мс fallback; seek-in-flight — прежнее поведение. Тесты должны падать на текущем коде.
- Вертикальный тест worker-а с fake-декодером: за N секунд модельного времени число тиков ≈ число кадров + аудио-подпитки, а не сотни в секунду.

## Замер и критерий выхода

- До/после `run_pass.py` HW H.264 1080p60, HW 4K30, SW H.264 1080p60 (worker общий). Ожидание: `player-worker` ≤ 0,3 %, `video-decode` падает на долю холостых повторов; в сводке тиков/с ≈ fps + аудио (десятки, не тысячи).
- Quality-gate README: уникальные кадры не падают (особенно проверить реальный 60 fps файл — открытый вопрос из PR #12), A/V-синхрон, seek/scrub, pause/resume, EOF → auto-next.
- Отчёт `results/02.md`, обновить `mem:player-core/core` (политика пробуждений) и `mem:video-vaapi/core`.
