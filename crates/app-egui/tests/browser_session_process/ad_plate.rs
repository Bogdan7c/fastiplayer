//! Плашка рекламы в браузерном окне (мост, сессия 09d, решения 184, 206, 207).
//!
//! Настоящий процесс под частной шиной и частным `kwin_wayland`; страницу играет тест.
//! Что доказывают тесты:
//! - `ad_state{showing, skip: available}` → строка журнала плашки с числами и видом
//!   пропуска; MPRIS `Next` (так жмёт робот стенда) → ровно один `skip_ad` на проводе,
//!   даже если нажать дважды; `ad_state{none}` → плашка снята (строка журнала);
//! - `skip: on_page` → строка журнала `skip=on_page`, `CanGoNext` false, `Next` ничего не
//!   отправляет странице (кнопку нажимает человек на странице, решение 208);
//! - пока реклама на экране, перемотка с шины не даёт `seek` на провод (и `CanGoNext`,
//!   `CanSeek` честно говорят, что можно), а после `none` даёт.
//!
//! Нужен `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::page_and_mpris::{
    MPRIS_BROWSER_NAME, MPRIS_PATH, MPRIS_PLAYER, browser_property, busctl, call_browser,
    wait_until_controllable,
};
use super::session_ends::{send_all, six_seconds_downloaded, wait_until_playing_past};
use super::viewer_notices::{fixture, generation_messages};
use super::*;

/// Строки журнала плашки (`crates/app-egui/src/ad_plate.rs`).
const PLATE_SHOWN_LINE: &str = "Ad plate shown";
const PLATE_CLEARED_LINE: &str = "Ad plate cleared";
const SKIP_SENT_LINE: &str = "Ad skip sent to the page";

/// Сколько ждём, что сообщения НЕ придут (команда, которой быть не должно).
const QUIET_PERIOD: Duration = Duration::from_millis(1_500);

fn ad_state_showing(index: u16, count: u16, skip: &str) -> Message {
    Message::text(format!(
        r#"{{"type":"ad_state","ad":{{"kind":"showing","pod":{{"kind":"known","index":{index},"count":{count}}},"skip":{{"kind":"{skip}"}}}}}}"#
    ))
}

fn ad_state_countdown(seconds: u16) -> Message {
    Message::text(format!(
        r#"{{"type":"ad_state","ad":{{"kind":"showing","pod":{{"kind":"unknown"}},"skip":{{"kind":"countdown","seconds":{seconds}}}}}}}"#
    ))
}

fn ad_state_none() -> Message {
    Message::text(r#"{"type":"ad_state","ad":{"kind":"none"}}"#)
}

/// Ждёт, пока свойство MPRIS окна станет `expected` («b true» / «b false»).
fn wait_for_property(bus: &PrivateBusWithBrowserSession, name: &str, expected: &str) {
    let deadline = Instant::now() + WAIT;
    while browser_property(bus, MPRIS_PLAYER, name) != expected {
        assert!(
            Instant::now() < deadline,
            "MPRIS {name} never became {expected}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Читает сообщения странице `period` и возвращает их (тишина — пустой список).
fn read_player_messages_for(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    period: Duration,
) -> Vec<PlayerSessionMessage> {
    let set_timeout = |socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, timeout| {
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(timeout))
                .expect("read timeout is set");
        }
    };
    let deadline = Instant::now() + period;
    let mut messages = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        set_timeout(socket, left.max(Duration::from_millis(10)));
        match socket.read() {
            Ok(Message::Text(text)) => {
                messages.push(decode_message(text.as_bytes()).expect("player session message"))
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => panic!("reading the page messages: {error}"),
        }
    }
    set_timeout(socket, WAIT);
    messages
}

fn count_of(messages: &[PlayerSessionMessage], wanted: &PlayerSessionMessage) -> usize {
    messages.iter().filter(|message| *message == wanted).count()
}

/// Окно с идущим видео и управляемым MPRIS.
fn playing_window(
    name: &str,
) -> Option<(
    SessionProcessUnderTest,
    WebSocket<MaybeTlsStream<TcpStream>>,
)> {
    let process = SessionProcessUnderTest::start(name, VideoMode::Window)?;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    wait_until_playing_past(&mut socket, 200_000);
    wait_until_controllable(&process.bus);
    Some((process, socket))
}

#[test]
fn mpris_next_presses_skip_once_and_the_plate_goes_away_with_the_ad() {
    let Some((process, mut socket)) = playing_window("ad-skip") else {
        return;
    };
    let bus = &process.bus;
    // Рекламы нет — «Next» окну нечего делать, как и раньше.
    assert_eq!(browser_property(bus, MPRIS_PLAYER, "CanGoNext"), "b false");

    socket
        .send(ad_state_showing(1, 2, "available"))
        .expect("ad_state is sent");
    let shown = bus.wait_for_log_line(PLATE_SHOWN_LINE);
    assert_eq!(log_number(&shown, "ad_index"), 1, "{shown}");
    assert_eq!(log_number(&shown, "ad_count"), 2, "{shown}");
    assert!(shown.contains("skip=skip"), "{shown}");
    wait_for_property(bus, "CanGoNext", "b true");

    call_browser(bus, MPRIS_PLAYER, "Next");
    // Второе нажатие сразу следом: кнопка занята (CanGoNext упал), команды не будет.
    call_browser(bus, MPRIS_PLAYER, "Next");
    bus.wait_for_log_line(SKIP_SENT_LINE);
    let mut seen = read_player_messages_until(&mut socket, "skip_ad", |message| {
        matches!(message, PlayerSessionMessage::SkipAd {})
    });
    seen.extend(read_player_messages_for(&mut socket, QUIET_PERIOD));
    assert_eq!(
        count_of(&seen, &PlayerSessionMessage::SkipAd {}),
        1,
        "two presses, one command"
    );

    // Страница ответила: рекламы нет — плашка снята, «Next» снова ничего не предлагает.
    socket.send(ad_state_none()).expect("ad_state is sent");
    bus.wait_for_log_line(PLATE_CLEARED_LINE);
    wait_for_property(bus, "CanGoNext", "b false");
}

#[test]
fn a_human_only_page_offers_no_button_and_mpris_next_sends_nothing() {
    let Some((process, mut socket)) = playing_window("ad-on-page") else {
        return;
    };
    let bus = &process.bus;
    socket
        .send(ad_state_showing(1, 1, "on_page"))
        .expect("ad_state is sent");
    let shown = bus.wait_for_log_line(PLATE_SHOWN_LINE);
    assert!(shown.contains("skip=on_page"), "{shown}");
    // Плашка даёт подсказку без кнопки: «Next» нечего нажимать.
    assert_eq!(browser_property(bus, MPRIS_PLAYER, "CanGoNext"), "b false");

    call_browser(bus, MPRIS_PLAYER, "Next");
    let seen = read_player_messages_for(&mut socket, QUIET_PERIOD);
    assert_eq!(
        count_of(&seen, &PlayerSessionMessage::SkipAd {}),
        0,
        "a human-only page is never clicked by us: {seen:?}"
    );
}

#[test]
fn a_countdown_offers_no_button_and_next_sends_nothing() {
    let Some((process, mut socket)) = playing_window("ad-countdown") else {
        return;
    };
    let bus = &process.bus;
    socket
        .send(ad_state_countdown(5))
        .expect("ad_state is sent");
    let shown = bus.wait_for_log_line(PLATE_SHOWN_LINE);
    assert!(shown.contains("skip=countdown"), "{shown}");
    assert_eq!(log_number(&shown, "countdown_seconds"), 5, "{shown}");
    assert_eq!(log_number(&shown, "ad_count"), 0, "no counter: {shown}");
    assert_eq!(browser_property(bus, MPRIS_PLAYER, "CanGoNext"), "b false");

    call_browser(bus, MPRIS_PLAYER, "Next");
    let seen = read_player_messages_for(&mut socket, QUIET_PERIOD);
    assert_eq!(
        count_of(&seen, &PlayerSessionMessage::SkipAd {}),
        0,
        "{seen:?}"
    );
}

#[test]
fn a_seek_from_the_bus_during_an_ad_never_reaches_the_page_and_works_after_it() {
    let Some(process) = SessionProcessUnderTest::start("ad-seek", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    // Объявлено 10 с, скачано 0,4 с; конца потока нет: перемотка к 5 с — вне полки, то
    // есть настоящий `seek` на провод, пока перемотка разрешена.
    send_all(
        &mut socket,
        generation_messages(
            1,
            10_000_000,
            fixture("video.mp4"),
            fixture("audio.mp4"),
            false,
        ),
    );
    read_player_messages_until(&mut socket, "a playing clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Playing,
                ..
            }
        )
    });
    wait_until_controllable(bus);

    socket
        .send(ad_state_countdown(10))
        .expect("ad_state is sent");
    bus.wait_for_log_line(PLATE_SHOWN_LINE);
    wait_for_property(bus, "CanSeek", "b false");

    // Seek с шины: перемотка закрыта, на провод ничего не уходит.
    let seek_micros = "5000000";
    let seek_args = [
        "call",
        MPRIS_BROWSER_NAME,
        MPRIS_PATH,
        MPRIS_PLAYER,
        "Seek",
        "x",
        seek_micros,
    ];
    busctl(bus, &seek_args).expect("MPRIS Seek call");
    let during = read_player_messages_for(&mut socket, QUIET_PERIOD);
    assert!(
        !during
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::Seek { .. })),
        "no seek during an ad: {during:?}"
    );

    // Реклама кончилась — та же перемотка снова доходит до страницы.
    socket.send(ad_state_none()).expect("ad_state is sent");
    bus.wait_for_log_line(PLATE_CLEARED_LINE);
    wait_for_property(bus, "CanSeek", "b true");
    busctl(bus, &seek_args).expect("MPRIS Seek call");
    read_player_messages_until(&mut socket, "the seek after the ad", |message| {
        matches!(message, PlayerSessionMessage::Seek { .. })
    });
}
