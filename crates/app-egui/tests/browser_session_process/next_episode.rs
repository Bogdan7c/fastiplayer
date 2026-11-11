//! Сквозные тесты 16a: следующая серия на той же странице (протокол 1.9) в настоящем
//! процессе с окном (частный `kwin_wayland`), до кадров на показе и звука.
//!
//! - **Другая спецификация:** серия 1 (64 px, звук моно 48 кГц, 6 с) доиграла, страница
//!   говорит `awaiting_next_episode` → окно пишет «Waiting for the next episode…»; серия 2
//!   приходит новым поколением с другим разрешением (96 px), другим звуком (стерео 44,1 кГц) и
//!   другой длительностью (4,8 с) → кадры показаны и звук записан ПОСЛЕ смены, MPRIS называет
//!   новую длительность, надпись снята показом поколения, окно то же.
//! - **Заголовок вкладки:** `page_title_changed` доходит до заголовка окна (глазами
//!   композитора) и до MPRIS, журнал видит только длину и номер смены; страница 1.8 этого
//!   слова не знает — сторож закрывает её связь кодом 4003.
//! - **Код 4011:** страница не начала следующую серию — надпись ожидания снята концом сеанса,
//!   окно говорит «The page didn't start the next episode» и закрывается без доигрывания.
//! - **Вкладка ушла во время ожидания:** надпись снята концом сеанса, дальше — обычное «Source
//!   tab closed».
//!
//! Нужны `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::generation_switch_clicks::{
    MPRIS_BROWSER_NAME, MPRIS_PATH, MPRIS_PLAYER, PLAYBACK_REFUSALS, PageSourceBuffers, busctl,
    is_ended, is_playing_from, split_fmp4, wait_until_controllable,
};
use super::hls_page::open_attached_session_speaking;
use super::page_ends::{NO_DRAIN_NO_FAREWELL, assert_no_return_to_page, page_closes_the_link};
use super::session_ends::{
    END_NOTICE_PERIOD_AT_LEAST, send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;

/// `hello` экстеншена протокола 1.9 (`awaiting_next_episode`, `page_title_changed`, 4011).
const HELLO_1_9: &str = r#"{"type":"hello","protocol":{"major":1,"minor":9},"software_version":"0.1.0","capabilities":[]}"#;

/// `hello` экстеншена протокола 1.8: слов 1.9 он не знает.
const HELLO_1_8: &str = r#"{"type":"hello","protocol":{"major":1,"minor":8},"software_version":"0.1.0","capabilities":[]}"#;

/// Слово страницы «ждём следующую серию» (срок экстеншена — решение 308).
const AWAITING_NEXT_EPISODE: &str = r#"{"type":"awaiting_next_episode","patience_ms":60000}"#;

/// Строки окна о надписи ожидания (договор со стендом моста).
const WAITING_SHOWN: &str = "Browser window tells the viewer: waiting for the next episode";
const WAITING_CLEARED: &str = "Browser window tells the viewer: the waiting notice is cleared";

/// Длительность серии 1 (`video-64-6s.mp4`, `audio-6s.mp4`) и серии 2 (четыре фрагмента
/// `video-96-6s.mp4` по 1,2 с и `audio-44k-stereo-4800ms.mp4`).
const FIRST_EPISODE_US: u64 = 6_000_000;
const SECOND_EPISODE_US: u64 = 4_800_000;
/// Фрагментов видео серии 2 (0–4,8 с).
const SECOND_EPISODE_VIDEO_FRAGMENTS: usize = 4;
/// Допуск длительности, которую называет плеер (кадр и хвост AAC-кодера).
const DURATION_SLACK_US: u64 = 100_000;

/// Кадров серии 2 (25 к/с) и сколько из них окно обязано показать после смены: запас на
/// установку поколения и первые кадры на занятой машине.
const SECOND_EPISODE_FRAMES: u64 = SECOND_EPISODE_US / 40_000;
const SECOND_EPISODE_FRAMES_AT_LEAST: u64 = SECOND_EPISODE_FRAMES - 40;
/// Сколько звука серии 2 обязано уйти в выход после смены (из 4,8 с; замер — 4,83 с).
/// Порог выше 4,41 с: столько насчитал бы счётчик, если бы кадры 44,1 кГц ушли в прежний
/// выход 48 кГц без пересборки (частота выхода не та, что у звука серии).
const SECOND_EPISODE_AUDIO_MS_AT_LEAST: u64 = 4_600;

/// Серия 1 целиком и конец её потока: страница докачала ролик, плеер доиграет до `ended`.
fn first_episode_to_the_end() -> Vec<Message> {
    let mut messages = six_seconds_downloaded();
    messages.push(Message::text(r#"{"type":"end_of_stream","generation":1}"#));
    messages
}

/// Серия 2 — поколение 2 с другой спецификацией (см. описание модуля) и концом потока.
fn second_episode_with_another_spec() -> Vec<Message> {
    let audio = split_fmp4("audio-44k-stereo-4800ms.mp4");
    let video = split_fmp4("video-96-6s.mp4");
    let mut page = PageSourceBuffers::new(2);
    let mut messages = vec![
        Message::text(r#"{"type":"generation_started","generation":2}"#),
        Message::text(format!(
            r#"{{"type":"duration_changed","generation":2,"duration":{{"kind":"finite","duration_us":{SECOND_EPISODE_US}}}}}"#
        )),
        Message::text(
            r#"{"type":"source_buffer_added","generation":2,"source_buffer":0,"mime_type":"audio/mp4; codecs=\"mp4a.40.2\""}"#,
        ),
        Message::text(
            r#"{"type":"source_buffer_added","generation":2,"source_buffer":1,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}"#,
        ),
        page.frame(0, &audio.init),
        page.frame(1, &video.init),
    ];
    for fragment in &audio.fragments {
        messages.push(page.frame(0, fragment));
    }
    for fragment in &video.fragments[..SECOND_EPISODE_VIDEO_FRAGMENTS] {
        messages.push(page.frame(1, fragment));
    }
    messages.push(Message::text(r#"{"type":"end_of_stream","generation":2}"#));
    messages
}

/// Слово страницы «заголовок вкладки сменился».
fn page_title_changed(title: &str) -> Message {
    Message::text(format!(
        r#"{{"type":"page_title_changed","title":"{title}"}}"#
    ))
}

/// Длительность, которую окно публикует в MPRIS (`mpris:length`, мкс); `None` — ещё нет.
fn mpris_length_us(bus: &PrivateBusWithBrowserSession) -> Option<u64> {
    let metadata = mpris_metadata(bus);
    let needle = "\"mpris:length\" x ";
    let start = metadata.find(needle)? + needle.len();
    metadata[start..]
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse().ok())
}

/// `Metadata` MPRIS окна как его печатает `busctl`.
fn mpris_metadata(bus: &PrivateBusWithBrowserSession) -> String {
    busctl(
        bus,
        &[
            "get-property",
            MPRIS_BROWSER_NAME,
            MPRIS_PATH,
            MPRIS_PLAYER,
            "Metadata",
        ],
    )
    .unwrap_or_else(|error| panic!("MPRIS Metadata: {error}"))
}

/// Ждёт, пока MPRIS назовёт длительность около `expected_us`; возвращает названную.
fn wait_for_mpris_length_near(bus: &PrivateBusWithBrowserSession, expected_us: u64) -> u64 {
    let deadline = Instant::now() + WAIT;
    loop {
        let length = mpris_length_us(bus);
        if length.is_some_and(|length| length.abs_diff(expected_us) <= DURATION_SLACK_US) {
            return length.unwrap_or_default();
        }
        assert!(
            Instant::now() < deadline,
            "MPRIS never reported a duration near {expected_us} µs, last {length:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Строка так, как её печатает `busctl`: байты вне ASCII — восьмеричными `\ooo`.
fn busctl_escaped(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii() {
                char::from(byte).to_string()
            } else {
                format!("\\{byte:03o}")
            }
        })
        .collect()
}

/// Ждёт, пока MPRIS назовёт заголовок с `title` (публикация — на проходе цикла).
fn wait_for_mpris_title(bus: &PrivateBusWithBrowserSession, title: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let metadata = mpris_metadata(bus);
        let expected = format!("\"xesam:title\" s \"{}\"", busctl_escaped(title));
        if metadata.contains(&expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "MPRIS never named the new title: {metadata}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Окно с сеансом страницы 1.9: серия 1 доиграла, страница сказала «ждём следующую серию»,
/// окно показало надпись. Возвращает сокет и строку журнала конца серии 1.
fn first_episode_played_and_awaiting(
    process: &SessionProcessUnderTest,
) -> (WebSocket<MaybeTlsStream<TcpStream>>, String) {
    let bus = &process.bus;
    let mut socket = open_attached_session_speaking(process, HELLO_1_9);
    bus.wait_for_log_line("Browser window created");
    send_all(&mut socket, first_episode_to_the_end());
    wait_until_playing_past(&mut socket, 1_000_000);
    read_player_messages_until(&mut socket, "the end of episode 1", is_ended);
    let first_end = bus.wait_for_log_line_without("Browser playback ended", &PLAYBACK_REFUSALS);
    socket
        .send(Message::text(AWAITING_NEXT_EPISODE))
        .expect("awaiting_next_episode is sent");
    bus.wait_for_log_line_without(WAITING_SHOWN, &PLAYBACK_REFUSALS);
    (socket, first_end)
}

/// Главное (решения 303, 306): следующая серия другой спецификации в том же окне — кадры и
/// звук после смены, новая длительность, надпись ожидания снята показом поколения.
#[test]
fn the_next_episode_with_another_spec_plays_in_the_same_window_after_the_waiting_notice() {
    let Some(process) = SessionProcessUnderTest::start("next-episode-spec", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let (mut socket, first_end) = first_episode_played_and_awaiting(&process);
    wait_until_controllable(bus);
    wait_for_mpris_length_near(bus, FIRST_EPISODE_US);
    // Повтор слова страницы надпись не «складывает»: второй строки о показе нет.
    socket
        .send(Message::text(AWAITING_NEXT_EPISODE))
        .expect("awaiting_next_episode is sent again");

    send_all(&mut socket, second_episode_with_another_spec());
    // Окно то же (второго «created» нет), надпись снята показом нового поколения.
    let mut forbidden = vec!["Browser window created", WAITING_SHOWN];
    forbidden.extend(PLAYBACK_REFUSALS);
    let cleared = bus.wait_for_log_line_without(WAITING_CLEARED, &forbidden);
    assert!(cleared.contains("reason=generation_shown"), "{cleared}");
    let after_switch = read_player_messages_until(&mut socket, "episode 2 playing", |message| {
        is_playing_from(message, 0)
    });
    assert!(
        !after_switch
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "episode 2 was rejected: {after_switch:?}"
    );
    read_player_messages_until(&mut socket, "the end of episode 2", is_ended);
    let second_end = bus.wait_for_log_line_without("Browser playback ended", &forbidden);

    // Кадры и звук серии 2 дошли до зрителя ПОСЛЕ смены (разница двух концов серий).
    assert_eq!(log_number(&second_end, "loads"), 2, "{second_end}");
    let frames_after_switch =
        log_number(&second_end, "presented_frames") - log_number(&first_end, "presented_frames");
    assert!(
        (SECOND_EPISODE_FRAMES_AT_LEAST..=SECOND_EPISODE_FRAMES + 5).contains(&frames_after_switch),
        "frames of episode 2: {frames_after_switch}\n{first_end}\n{second_end}"
    );
    let audio_after_switch =
        log_number(&second_end, "audio_written_ms") - log_number(&first_end, "audio_written_ms");
    assert!(
        audio_after_switch >= SECOND_EPISODE_AUDIO_MS_AT_LEAST,
        "audio of episode 2 (44.1 kHz stereo): {audio_after_switch} ms\n{first_end}\n{second_end}"
    );
    // Плеер называет длительность новой серии, а не прежние 6 с.
    let length = wait_for_mpris_length_near(bus, SECOND_EPISODE_US);
    assert!(length.abs_diff(FIRST_EPISODE_US) > DURATION_SLACK_US);

    send_tab_gone_and_expect_close(&mut socket);
    bus.wait_for_log_line_without("Browser session process finished", &forbidden);
}

/// `page_title_changed` (16a): новый заголовок доходит до заголовка окна и MPRIS; журнал
/// видит только длину в байтах и номер смены.
#[test]
fn a_page_title_change_reaches_the_window_title_and_the_log_shows_only_numbers() {
    let Some(process) = SessionProcessUnderTest::start("page-retitle", VideoMode::Window) else {
        return;
    };
    let display = process.display().expect("window mode has a display");
    let bus = &process.bus;
    // Кириллица: байт больше, чем символов; уникальный хвост ловит утечку в журнал.
    let first_title = "Серія 4 Q8X";
    let second_title = "Серія 5 Q8X";
    bus.forbid_in_log(&["Q8X", "Серія"]);
    let mut socket = open_attached_session_speaking(&process, HELLO_1_9);
    send_all(&mut socket, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut socket, 500_000);
    wait_until_controllable(bus);

    socket
        .send(page_title_changed(first_title))
        .expect("title is sent");
    let first = bus.wait_for_log_line("Browser page title changed");
    assert!(
        first.contains(&format!("title_bytes={} revision=1", first_title.len())),
        "{first}"
    );
    wait_for_mpris_title(bus, first_title);
    socket
        .send(page_title_changed(second_title))
        .expect("title is sent");
    let second = bus.wait_for_log_line("Browser page title changed");
    assert!(
        second.contains(&format!("title_bytes={} revision=2", second_title.len())),
        "{second}"
    );
    wait_for_mpris_title(bus, second_title);

    // Закрывается только окно с ТОЧНО таким заголовком: закрытие и есть доказательство.
    display.close_fastiplayer_window_titled(&format!("{second_title} — example.org"));
    let (_last_clock, _farewell, code) =
        super::session_ends::read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    bus.wait_for_log_line("Browser session process finished");
}

/// Страница 1.8 слова 1.9 знать не может: сторож протокола закрывает связь кодом 4003, а
/// медиа-часть заголовка не видит.
#[test]
fn a_page_title_change_from_a_protocol_1_8_page_closes_the_session() {
    let Some(process) = SessionProcessUnderTest::start("page-retitle-1-8", VideoMode::Headless)
    else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_8);
    socket
        .send(page_title_changed("Too new"))
        .expect("title is sent");
    assert_eq!(
        read_close_code(&mut socket),
        CloseCode::from(SessionCloseReason::ProtocolViolation.close_code())
    );
    process.bus.wait_for_log_line_without(
        "Browser session process finished",
        &["Browser page title changed"],
    );
}

/// Код 4011 (решение 307): страница не начала следующую серию — надпись ожидания снята
/// концом сеанса, окно говорит об этом и закрывается без доигрывания и прощания.
#[test]
fn the_page_not_starting_the_next_episode_is_told_then_the_window_closes() {
    let Some(process) = SessionProcessUnderTest::start("next-episode-4011", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let (mut socket, _first_end) = first_episode_played_and_awaiting(&process);

    let after_close = page_closes_the_link(&mut socket, SessionCloseReason::NextEpisodeNotStarted);
    assert_no_return_to_page(&after_close);

    let ended = bus.wait_for_log_line_without("Browser session ended", NO_DRAIN_NO_FAREWELL);
    let close_code = SessionCloseReason::NextEpisodeNotStarted.close_code();
    assert!(
        ended.contains(&format!("ClosedByPage {{ code: Some({close_code}) }}")),
        "{ended}"
    );
    let cleared = bus.wait_for_log_line_without(WAITING_CLEARED, NO_DRAIN_NO_FAREWELL);
    assert!(cleared.contains("reason=session_ended"), "{cleared}");
    let notice =
        bus.wait_for_log_line_without("Browser session end notice shown", NO_DRAIN_NO_FAREWELL);
    let notice_at = Instant::now();
    assert!(
        notice.contains("notice=next_episode_not_started cause=next_episode_not_started end=none"),
        "{notice}"
    );
    bus.wait_for_log_line_without(
        "Browser window closed after the end notice",
        NO_DRAIN_NO_FAREWELL,
    );
    let shown_for = notice_at.elapsed();
    assert!(
        shown_for >= END_NOTICE_PERIOD_AT_LEAST,
        "the window closed {shown_for:?} after the notice"
    );
    bus.wait_for_log_line_without("Browser session process finished", NO_DRAIN_NO_FAREWELL);
}

/// Вкладка ушла, пока страница ждала следующую серию: надпись снята концом сеанса, дальше —
/// обычный конец закрытой вкладки (досматривать нечего — серия уже доиграла).
#[test]
fn a_tab_gone_while_awaiting_the_next_episode_clears_the_waiting_notice() {
    let Some(process) = SessionProcessUnderTest::start("next-episode-tab-gone", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let (mut socket, _first_end) = first_episode_played_and_awaiting(&process);

    send_tab_gone_and_expect_close(&mut socket);
    let cleared = bus.wait_for_log_line(WAITING_CLEARED);
    assert!(cleared.contains("reason=session_ended"), "{cleared}");
    let notice = bus.wait_for_log_line("Browser session end notice shown");
    assert!(
        notice.contains("notice=finished cause=tab_gone"),
        "{notice}"
    );
    bus.wait_for_log_line("Browser session process finished");
}
