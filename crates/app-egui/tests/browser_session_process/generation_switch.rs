//! Сквозной тест 03e: смена качества новым `MediaSource` три раза подряд.
//!
//! Запись `quality-switch-new-media-source` повторяет живую смену качества YouTube:
//! каждое новое поколение страница объявляет, кладёт init-сегменты, сообщает позицию
//! зрителя (`seek_started{page_seek}`) и только потом кладёт коробки — видео с
//! ключевого кадра ДО позиции, звук с ещё более раннего фрагмента. Паузы записи
//! (`await_clock_us`) — смена посреди просмотра: тест ждёт часы плеера.
//!
//! До 03e процесс выбрасывал перемотку страницы, пришедшую, пока новое поколение
//! открывалось, и ставил его с нуля: часы шли назад, а на живом YouTube плеер вставал.
//!
//! Пока новое поколение ставится, прежнее играет дальше; поставленное встаёт на позицию
//! страницы — часы отступают ровно на время установки (известное ограничение 03e, у
//! часов нет номера поколения). Ключевые кадры записи — раз в 1,2 с: старт «с первой
//! коробки» вместо позиции страницы отбросил бы часы почти на секунду.

use super::*;

/// Запись в фикстурах протокола.
const RECORDING: &str = "quality-switch-new-media-source";
/// Позиции зрителя, которые страница называет при сменах качества (как в записи).
const PAGE_POSITIONS_US: [u64; 3] = [2_200_000, 3_500_000, 4_700_000];
/// Насколько часы «играет» могут отступить на смене: прежнее поколение играет, пока
/// новое ставится (отладочная сборка — до ~0,35 с). Старт с первой коробки вместо
/// позиции страницы отступил бы на ~1 с.
const MAX_INSTALL_STEP_BACK_US: u64 = 600_000;
/// Длина ролика записи.
const CLIP_US: u64 = 6_000_000;
/// Кадр ролика (25 к/с).
const FRAME_US: u64 = 40_000;
/// Часы странице идут 4 раза в секунду: после старта с позиции страницы часы с
/// позицией из этого окна обязаны прийти (с запасом на шаг часов и установку).
const START_WINDOW_US: u64 = 400_000;

/// Позиции часов «играет» по порядку прихода.
fn playing_clock_positions(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect()
}

/// Проигрывает запись: сообщения и рамки — на провод, паузы — ждать часов плеера.
/// Возвращает всё, что страница получила по дороге.
fn play_recording(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    steps: Vec<StreamStep>,
) -> Vec<PlayerSessionMessage> {
    let mut received = Vec::new();
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
            StreamStep::AwaitClock(position_us) => {
                received.extend(read_player_messages_until(
                    socket,
                    "the clock the recording waited for",
                    |message| {
                        matches!(
                            message,
                            PlayerSessionMessage::Clock {
                                position_us: clock,
                                state: ClockState::Playing,
                                ..
                            } if clock.as_micros() >= position_us
                        )
                    },
                ));
                continue;
            }
        };
        socket.send(message).expect("stream step is sent");
    }
    received
}

/// Главное: после каждой смены `MediaSource` видео играет дальше с позиции, которую
/// назвала страница, — время у страницы не идёт назад, ролик доигрывается до конца,
/// кадры и звук доходят до зрителя.
#[test]
fn three_media_source_switches_in_a_row_keep_playing_from_the_page_position() {
    let bus = PrivateBusWithBrowserSession::start("quality-switch");
    let mut socket = open_attached_session(&bus);
    let (steps, expected_bytes) = recorded_stream(RECORDING);

    let mut messages = play_recording(&mut socket, steps);
    messages.extend(read_player_messages_until(
        &mut socket,
        "the clock state ended",
        |message| {
            matches!(
                message,
                PlayerSessionMessage::Clock {
                    state: ClockState::Ended,
                    ..
                }
            )
        },
    ));

    // 1. Время «играет» не уходит назад дальше задержки установки: каждое новое
    //    поколение стартует с позиции страницы (без неё — к нулю или к первой коробке).
    let playing = playing_clock_positions(&messages);
    assert!(
        playing
            .windows(2)
            .all(|pair| pair[0] <= pair[1] + MAX_INSTALL_STEP_BACK_US),
        "the page clock went backwards past the install delay: {playing:?}"
    );
    // 2. После каждой смены часы проходят через позицию страницы: видео играет оттуда.
    for page_position in PAGE_POSITIONS_US {
        assert!(
            playing
                .iter()
                .any(|&position| (page_position..page_position + START_WINDOW_US)
                    .contains(&position)),
            "no playing clock right after the page position {page_position}: {playing:?}"
        );
    }
    // 3. Ролик доигран до конца.
    let ended_at = messages.iter().find_map(|message| match message {
        PlayerSessionMessage::Clock {
            position_us,
            state: ClockState::Ended,
            ..
        } => Some(position_us.as_micros()),
        _ => None,
    });
    assert!(
        ended_at.is_some_and(|position| position + FRAME_US >= CLIP_US),
        "the clip did not play to its end: {ended_at:?}"
    );
    bus.wait_for_log_line("Browser playback ended");

    // 4. Зритель видел и слышал: кадров — почти весь ролик (пропущено только то, что
    //    страница перешагнула на сменах, ≤ 0,5 с на смену), звук не вставал.
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    let presented = log_number(&summary, "presented_frames");
    let clip_frames = CLIP_US / FRAME_US;
    assert!(
        (clip_frames - 40..=clip_frames).contains(&presented),
        "presented frames out of range: {summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    assert_eq!(log_number(&summary, "loads"), 4, "{summary}");
    assert!(
        log_number(&summary, "audio_played_ms") >= 4_000,
        "{summary}"
    );
    let totals = bus.wait_for_log_line("Browser session stopped");
    assert!(
        totals.contains(&format!("received_bytes={expected_bytes}")),
        "expected {expected_bytes} bytes in: {totals}"
    );
    bus.wait_for_log_line("Browser session process finished");
}
