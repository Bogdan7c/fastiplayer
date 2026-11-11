//! Сквозные тесты 16a2: `play` страницы после конца серии не повторяет её с начала.
//!
//! Серия 1 доиграла (`ended`), страница берёт следующую серию тем же элементом и говорит
//! `page_playback{play}`. Раньше плеер отдавал этот `play` доигравшему поколению, и ядро
//! исполняло его повтором с начала: зритель видел и слышал начало ПРОШЛОЙ серии, пока
//! новая не встала. Оба порядка живого журнала:
//! - `play` раньше объявления новой серии (у MSE объявление — `source_buffer_added`);
//! - объявление, потом `page_seek{0}` + `play`, потом данные.
//!
//! Между `play` и данными серии 2 тест нарочно держит паузу [`GAP_BEFORE_THE_NEXT_STEP`]:
//! повтор с начала успел бы показать ~37 кадров и выдать ~1,5 с звука серии 1 — счётчики
//! журнала это видят. Доказательства (каждое падает без исправления):
//! - страница 1.10 не получает ни одного тика серии 1 (`generation: 1`) кроме `ended` после
//!   её конца — повтор дал бы `playing` около нуля;
//! - между концами двух серий показано и сыграно ровно столько, сколько в серии 2;
//! - серия 2 играет: её тики несут номер 2, MPRIS называет её длительность.
//!
//! Повтор той же серии перемоткой страницы проверяют фокусные тесты медиа-части
//! (`session_media/tests/page_play_after_end.rs`). Нужны `kwin_wayland` (иначе SKIPPED) и
//! `busctl`.

use super::generation_switch_clicks::{PLAYBACK_REFUSALS, is_ended, wait_until_controllable};
use super::hls_page::open_attached_session_speaking;
use super::next_episode::{
    AWAITING_NEXT_EPISODE, DURATION_SLACK_US, FIRST_EPISODE_US, SECOND_EPISODE_AUDIO_MS_AT_LEAST,
    SECOND_EPISODE_FRAMES, SECOND_EPISODE_FRAMES_AT_LEAST, SECOND_EPISODE_US, WAITING_CLEARED,
    WAITING_SHOWN, first_episode_to_the_end, second_episode_with_another_spec,
    wait_for_mpris_length_near,
};
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// `hello` экстеншена протокола 1.10: тики несут номер поколения.
pub(super) const HELLO_1_10: &str = r#"{"type":"hello","protocol":{"major":1,"minor":10},"software_version":"0.1.0","capabilities":[]}"#;

/// `play` страницы (как его шлёт тень после `ended` на новом источнике).
const PAGE_PLAY: &str = r#"{"type":"page_playback","request":{"kind":"play"}}"#;

/// Перемотка страницы в начало нового источника.
const PAGE_SEEK_TO_ZERO: &str =
    r#"{"type":"seek_started","target_us":0,"cause":{"kind":"page_seek"}}"#;

/// Пауза между `play` и следующим шагом: за неё повтор с начала показал бы ~37 кадров и
/// ~1,5 с звука серии 1 — заметно больше допусков счётчиков ниже.
const GAP_BEFORE_THE_NEXT_STEP: Duration = Duration::from_millis(1_500);

/// Сообщений объявления серии 2 в начале [`second_episode_with_another_spec`]:
/// `generation_started`, `duration_changed` и два `source_buffer_added`.
const SECOND_EPISODE_ANNOUNCEMENT: usize = 4;

/// Сколько звука серии 2 может уйти в выход сверх её длины (хвост AAC и кадр запаса; замер
/// next_episode — 4,83 с из 4,8 с). Повтор серии 1 добавил бы ~1,5 с.
const SECOND_EPISODE_AUDIO_MS_AT_MOST: u64 = SECOND_EPISODE_US / 1_000 + 250;

/// Номер поколения тика (`None` — не тик или тик без номера).
fn clock_generation(message: &PlayerSessionMessage) -> Option<u32> {
    match message {
        PlayerSessionMessage::Clock { generation, .. } => generation.map(StreamGeneration::get),
        _ => None,
    }
}

/// Тик серии 2 «играю».
fn is_second_episode_playing(message: &PlayerSessionMessage) -> bool {
    matches!(
        message,
        PlayerSessionMessage::Clock {
            state: ClockState::Playing,
            ..
        }
    ) && clock_generation(message) == Some(2)
}

/// Читает сообщения плеера в течение `period` (часы идут 4 раза в секунду, чтение не висит).
fn read_player_messages_for(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    period: Duration,
) -> Vec<PlayerSessionMessage> {
    let until = Instant::now() + period;
    read_player_messages_until(socket, "a clock during the gap", |_| {
        Instant::now() >= until
    })
}

/// Окно с сеансом страницы 1.10: серия 1 доиграла, страница сказала «ждём следующую серию».
/// Возвращает сокет и строку журнала конца серии 1.
fn first_episode_played_and_awaiting(
    process: &SessionProcessUnderTest,
) -> (WebSocket<MaybeTlsStream<TcpStream>>, String) {
    let bus = &process.bus;
    let mut socket = open_attached_session_speaking(process, HELLO_1_10);
    bus.wait_for_log_line("Browser window created");
    send_all(&mut socket, first_episode_to_the_end());
    wait_until_playing_past(&mut socket, 1_000_000);
    read_player_messages_until(&mut socket, "the end of episode 1", is_ended);
    let first_end = bus.wait_for_log_line_without("Browser playback ended", &PLAYBACK_REFUSALS);
    socket
        .send(Message::text(AWAITING_NEXT_EPISODE))
        .expect("awaiting_next_episode is sent");
    bus.wait_for_log_line_without(WAITING_SHOWN, &PLAYBACK_REFUSALS);
    wait_until_controllable(bus);
    wait_for_mpris_length_near(bus, FIRST_EPISODE_US);
    (socket, first_end)
}

/// Серия 2 доиграла после смены: что видела страница с конца серии 1 (`after_first_end`)
/// и что насчитал журнал между концами серий. Общие проверки обоих порядков.
fn assert_only_the_second_episode_played(
    process: &SessionProcessUnderTest,
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    first_end: &str,
    mut after_first_end: Vec<PlayerSessionMessage>,
) {
    let bus = &process.bus;
    let mut forbidden = vec!["Browser window created", WAITING_SHOWN];
    forbidden.extend(PLAYBACK_REFUSALS);
    let cleared = bus.wait_for_log_line_without(WAITING_CLEARED, &forbidden);
    assert!(cleared.contains("reason=generation_shown"), "{cleared}");
    after_first_end.extend(read_player_messages_until(
        socket,
        "episode 2 playing",
        is_second_episode_playing,
    ));
    after_first_end.extend(read_player_messages_until(
        socket,
        "the end of episode 2",
        is_ended,
    ));
    let second_end = bus.wait_for_log_line_without("Browser playback ended", &forbidden);

    // Серия 1 после своего конца странице только «кончилась»: повтор дал бы `playing` у 0.
    let first_episode_ticks: Vec<&PlayerSessionMessage> = after_first_end
        .iter()
        .filter(|message| clock_generation(message) == Some(1))
        .collect();
    assert!(
        !first_episode_ticks.is_empty(),
        "the page heard no numbered tick of episode 1 after its end"
    );
    assert!(
        first_episode_ticks.iter().all(|message| is_ended(message)),
        "episode 1 played again after its end: {first_episode_ticks:?}"
    );
    assert!(
        !after_first_end
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "episode 2 was rejected: {after_first_end:?}"
    );

    // Между концами серий — кадры и звук ровно серии 2 (повтор серии 1 их прибавил бы).
    assert_eq!(log_number(&second_end, "loads"), 2, "{second_end}");
    // Повтор с начала — откат позиции внутри медиа серии 1; смена серии откатом не считается.
    assert_eq!(
        log_number(&second_end, "position_regressions"),
        log_number(first_end, "position_regressions"),
        "{first_end}\n{second_end}"
    );
    let frames_between_ends =
        log_number(&second_end, "presented_frames") - log_number(first_end, "presented_frames");
    assert!(
        (SECOND_EPISODE_FRAMES_AT_LEAST..=SECOND_EPISODE_FRAMES + 5).contains(&frames_between_ends),
        "frames between the two ends: {frames_between_ends}\n{first_end}\n{second_end}"
    );
    let audio_between_ends =
        log_number(&second_end, "audio_written_ms") - log_number(first_end, "audio_written_ms");
    assert!(
        (SECOND_EPISODE_AUDIO_MS_AT_LEAST..=SECOND_EPISODE_AUDIO_MS_AT_MOST)
            .contains(&audio_between_ends),
        "audio between the two ends: {audio_between_ends} ms\n{first_end}\n{second_end}"
    );
    let length = wait_for_mpris_length_near(bus, SECOND_EPISODE_US);
    assert!(length.abs_diff(FIRST_EPISODE_US) > DURATION_SLACK_US);

    send_tab_gone_and_expect_close(socket);
    bus.wait_for_log_line_without("Browser session process finished", &forbidden);
}

/// Порядок 1: `play` страницы раньше объявления новой серии, данные — после паузы.
#[test]
fn a_page_play_before_the_next_episode_is_announced_does_not_replay_the_finished_one() {
    let Some(process) = SessionProcessUnderTest::start("play-before-announce", VideoMode::Window)
    else {
        return;
    };
    let (mut socket, first_end) = first_episode_played_and_awaiting(&process);

    socket.send(Message::text(PAGE_PLAY)).expect("play is sent");
    let after_first_end = read_player_messages_for(&mut socket, GAP_BEFORE_THE_NEXT_STEP);
    send_all(&mut socket, second_episode_with_another_spec());

    assert_only_the_second_episode_played(&process, &mut socket, &first_end, after_first_end);
}

/// Порядок 2: объявление новой серии, затем `page_seek{0}` + `play`, данные — после паузы.
#[test]
fn a_page_seek_and_play_after_the_announcement_start_the_next_episode_not_a_replay() {
    let Some(process) = SessionProcessUnderTest::start("play-after-announce", VideoMode::Window)
    else {
        return;
    };
    let (mut socket, first_end) = first_episode_played_and_awaiting(&process);
    let mut second_episode = second_episode_with_another_spec();
    let second_episode_data = second_episode.split_off(SECOND_EPISODE_ANNOUNCEMENT);

    send_all(&mut socket, second_episode);
    send_all(
        &mut socket,
        vec![Message::text(PAGE_SEEK_TO_ZERO), Message::text(PAGE_PLAY)],
    );
    let after_first_end = read_player_messages_for(&mut socket, GAP_BEFORE_THE_NEXT_STEP);
    send_all(&mut socket, second_episode_data);

    assert_only_the_second_episode_played(&process, &mut socket, &first_end, after_first_end);
}
