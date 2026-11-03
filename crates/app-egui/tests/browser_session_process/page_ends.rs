//! Честные концы, когда связь закрывает страница (мост, сессия 09a2, Д-3 и Д-6).
//!
//! Экстеншен закрывает сокет сам в двух осмысленных случаях, и окно обязано сказать о
//! них человеку, а не исчезать молча:
//! - **4009 `page_took_video_back`** (решение 179): страница честно забрала видео, вкладка
//!   на месте. Окно пишет «The page took the video back», держит сообщение срок
//!   сообщения и закрывается. Без доигрывания (страница уже играет сама — было бы два
//!   звука) и без прощания `return_to_page` (видео и так у страницы).
//! - **4003 страницы** (сбой на стороне страницы, решение 132): тот же вид, что у
//!   сбоя со стороны плеера, — «Stream error — video returned to the page», но с
//!   причиной `page_stream_error`. Прощания тоже нет: сокет уже закрыт страницей.
//!
//! Без окна процесс выходит сразу, без доигрывания (правило 103).
//! Оконные варианты идут под частным `kwin_wayland --virtual` и печатают SKIPPED без него.

use tungstenite::protocol::CloseFrame;

use super::session_ends::{
    END_NOTICE_PERIOD_AT_LEAST, send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;

/// Строки журнала, которых при честном конце со стороны страницы быть не должно:
/// доигрывание (решение 179: страница уже играет сама) и прощание `return_to_page`
/// (сокет закрыла страница, видео и так у неё).
const NO_DRAIN_NO_FAREWELL: &[&str] = &[
    "Playback drain finished",
    "Session closed with a farewell to the page",
];

/// Сколько без окна процесс может идти к выходу после закрытия связи страницей. Доигрывание
/// играло бы ещё ~5,5 с скачанного (ролик 6 с, закрытие после 0,5 с), а «сразу» на
/// занятой машине — доли секунды: 3 с надёжно разделяют эти два исхода.
const HEADLESS_EXIT_WITHIN: Duration = Duration::from_secs(3);

/// Страница закрывает связь кодом причины `reason` (как экстеншен) и дочитывает сокет до
/// ответного кадра закрытия. Возвращает сообщения плеера, пришедшие после закрытия: туда
/// могут попасть только часы, отправленные до того, как плеер прочёл наш кадр.
fn page_closes_the_link(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    reason: SessionCloseReason,
) -> Vec<PlayerSessionMessage> {
    socket
        .close(Some(CloseFrame {
            code: CloseCode::from(reason.close_code()),
            reason: "".into(),
        }))
        .expect("the page sends its close frame");
    let deadline = Instant::now() + WAIT;
    let mut after_close = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "the player never answered the close frame"
        );
        match socket.read() {
            Ok(Message::Text(text)) => after_close.push(
                decode_message::<PlayerSessionMessage>(text.as_bytes())
                    .expect("player session message"),
            ),
            // Ответный кадр закрытия: после него плеер не шлёт ничего (RFC 6455).
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                return after_close;
            }
            Ok(_) => continue,
            Err(error) => panic!("waiting for the close answer: {error}"),
        }
    }
}

/// Плеер ничего не вернул странице: среди сообщений после закрытия нет `return_to_page`.
fn assert_no_return_to_page(after_close: &[PlayerSessionMessage]) {
    assert!(
        !after_close
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::ReturnToPage { .. })),
        "the page already holds the video, nothing is returned to it: {after_close:?}"
    );
}

/// Как окно должно закончить сеанс, закрытый страницей.
struct ExpectedPageEnd {
    /// Код, которым страница закрывает связь.
    closed_with: SessionCloseReason,
    /// Метки строки журнала окна (договор со стендом, пакет E).
    notice_labels: &'static str,
}

const PAGE_TOOK_VIDEO_BACK: ExpectedPageEnd = ExpectedPageEnd {
    closed_with: SessionCloseReason::PageTookVideoBack,
    notice_labels: "notice=page_took_video_back cause=page_took_video_back end=none",
};

const PAGE_STREAM_ERROR: ExpectedPageEnd = ExpectedPageEnd {
    closed_with: SessionCloseReason::ProtocolViolation,
    notice_labels: "notice=stream_error cause=page_stream_error end=none",
};

#[test]
fn the_page_takes_the_video_back_and_the_window_says_so_then_closes() {
    the_window_says_how_the_page_ended_the_session("page-took-video-back", &PAGE_TOOK_VIDEO_BACK);
}

#[test]
fn a_stream_error_on_the_page_side_is_not_a_silent_end_of_the_window() {
    the_window_says_how_the_page_ended_the_session("page-stream-error", &PAGE_STREAM_ERROR);
}

fn the_window_says_how_the_page_ended_the_session(name: &str, expected: &ExpectedPageEnd) {
    let Some(process) = SessionProcessUnderTest::start(name, VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    // Окно играет поток страницы: часы идут, кадры на показе.
    wait_until_playing_past(&mut socket, 500_000);

    let after_close = page_closes_the_link(&mut socket, expected.closed_with);
    assert_no_return_to_page(&after_close);

    // Процесс понял, кто и каким кодом закрыл связь.
    let ended = bus.wait_for_log_line_without("Browser session ended", NO_DRAIN_NO_FAREWELL);
    let close_code = expected.closed_with.close_code();
    assert!(
        ended.contains(&format!("ClosedByPage {{ code: Some({close_code}) }}")),
        "{ended}"
    );
    // Окно говорит, что случилось, а не исчезает молча.
    let notice =
        bus.wait_for_log_line_without("Browser session end notice shown", NO_DRAIN_NO_FAREWELL);
    let notice_at = Instant::now();
    assert!(notice.contains(expected.notice_labels), "{notice}");
    // Сообщение держится срок сообщения, потом окно закрывается само.
    bus.wait_for_log_line_without(
        "Browser window closed after the end notice",
        NO_DRAIN_NO_FAREWELL,
    );
    let shown_for = notice_at.elapsed();
    assert!(
        shown_for >= END_NOTICE_PERIOD_AT_LEAST,
        "the window closed {shown_for:?} after the notice"
    );
    // Штатный выход (код 0 — `Completed`), по дороге — ни доигрывания, ни прощания.
    bus.wait_for_log_line_without("Browser session process finished", NO_DRAIN_NO_FAREWELL);
}

#[test]
fn without_a_window_the_page_taking_the_video_back_ends_the_process_at_once() {
    let Some(process) = SessionProcessUnderTest::start("page-took-video-back", VideoMode::Headless)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    wait_until_playing_past(&mut socket, 500_000);

    let after_close = page_closes_the_link(&mut socket, SessionCloseReason::PageTookVideoBack);
    let closed_at = Instant::now();
    assert_no_return_to_page(&after_close);

    bus.wait_for_log_line_without("Browser session process finished", NO_DRAIN_NO_FAREWELL);
    let exited_after = closed_at.elapsed();
    assert!(
        exited_after < HEADLESS_EXIT_WITHIN,
        "the process played the download on for {exited_after:?} instead of exiting at once"
    );
}
