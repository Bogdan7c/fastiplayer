//! Домен и заголовок страницы в окне и MPRIS окна (мост, сессия 04c).
//!
//! Что доказывают тесты (настоящий процесс под частной шиной и частным `kwin_wayland`):
//! - заголовок окна глазами композитора ровно «<заголовок> — <домен>», а в журнале
//!   процесса ни домена, ни заголовка нет ни в одной строке (инвариант 7);
//! - окно занимает `org.mpris.MediaPlayer2.fastiplayer.browser` и НЕ занимает имя
//!   обычного плеера; `Pause`/`Play` с шины доходят до страницы командами `pause`/`play`
//!   (тем же путём, что кнопка окна), `Quit` закрывает окно с прощанием.
//!
//! Нужен `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::session_ends::{
    read_farewell_then_close, send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;

const MPRIS_BROWSER_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer.browser";
const MPRIS_STANDARD_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const MPRIS_ROOT: &str = "org.mpris.MediaPlayer2";

/// Данные страницы, которых не должно быть в журнале.
const PAGE_TITLE: &str = "Cats unique-title-x";
const PAGE_DOMAIN: &str = "unique-site.example";

/// Заголовок окна, как его должен показать композитор.
const EXPECTED_CAPTION: &str = "Cats unique-title-x — unique-site.example";

fn open_session_of_the_page() -> String {
    format!(
        r#"{{"type":"open_session","request_id":1,"session_key":"tab-77-video-1","page":{{"title":"{PAGE_TITLE}","domain":"{PAGE_DOMAIN}"}},"intent":"window","hints":{{"duration":{{"kind":"unknown"}},"position":{{"kind":"unknown"}}}}}}"#
    )
}

/// Вызов `busctl` на частной шине; `Ok(stdout)` или текст ошибки.
fn busctl(bus: &PrivateBusWithBrowserSession, args: &[&str]) -> Result<String, String> {
    let output = Command::new("busctl")
        .arg(format!("--address={}", bus.address))
        .arg("--no-pager")
        .args(args)
        .output()
        .expect("busctl must be installed for the MPRIS tests");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Метод интерфейса MPRIS окна.
fn call_browser(bus: &PrivateBusWithBrowserSession, interface: &str, method: &str) {
    busctl(
        bus,
        &["call", MPRIS_BROWSER_NAME, MPRIS_PATH, interface, method],
    )
    .unwrap_or_else(|error| panic!("MPRIS {method} failed: {error}"));
}

/// Свойство (`busctl get-property` печатает «тип значение»).
fn browser_property(bus: &PrivateBusWithBrowserSession, interface: &str, name: &str) -> String {
    busctl(
        bus,
        &[
            "get-property",
            MPRIS_BROWSER_NAME,
            MPRIS_PATH,
            interface,
            name,
        ],
    )
    .unwrap_or_else(|error| panic!("MPRIS property {name}: {error}"))
    .trim()
    .to_owned()
}

/// Ждёт, пока MPRIS окна опубликует управляемый вид (кнопки появляются вместе со страницей).
fn wait_until_controllable(bus: &PrivateBusWithBrowserSession) {
    let deadline = Instant::now() + WAIT;
    loop {
        let ready = busctl(
            bus,
            &[
                "get-property",
                MPRIS_BROWSER_NAME,
                MPRIS_PATH,
                MPRIS_PLAYER,
                "CanPause",
            ],
        )
        .is_ok_and(|text| text.trim() == "b true");
        if ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the browser window MPRIS never became controllable"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_window_owns_its_own_mpris_name_and_leaves_the_standard_players_alone() {
    let Some(process) = SessionProcessUnderTest::start("mpris-names", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    wait_until_playing_past(&mut socket, 200_000);
    wait_until_controllable(bus);

    let names = busctl(bus, &["list"]).expect("bus names");
    let owned = |name: &str| {
        names
            .lines()
            .any(|line| line.split_whitespace().next() == Some(name))
    };
    assert!(owned(MPRIS_BROWSER_NAME), "{names}");
    assert!(
        !owned(MPRIS_STANDARD_NAME),
        "the browser process must not take the standard player's name: {names}"
    );
    assert_eq!(
        browser_property(bus, MPRIS_ROOT, "Identity"),
        "s \"Fastiplayer (browser)\""
    );
    assert_eq!(browser_property(bus, MPRIS_PLAYER, "CanGoNext"), "b false");
    assert_eq!(
        browser_property(bus, MPRIS_PLAYER, "CanGoPrevious"),
        "b false"
    );
    assert_eq!(browser_property(bus, MPRIS_ROOT, "CanQuit"), "b true");
    // Страница теста — «Episode 3» на `example.org` (`OPEN_SESSION` набора).
    let metadata = browser_property(bus, MPRIS_PLAYER, "Metadata");
    assert!(
        metadata.contains("Episode 3") && metadata.contains("example.org"),
        "the page title and domain are the track title and album: {metadata}"
    );
}

#[test]
fn pause_and_play_from_the_bus_reach_the_page_as_commands() {
    let Some(process) = SessionProcessUnderTest::start("mpris-transport", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    wait_until_playing_past(&mut socket, 200_000);
    wait_until_controllable(bus);

    call_browser(bus, MPRIS_PLAYER, "Pause");
    read_player_messages_until(&mut socket, "the pause command from the bus", |message| {
        matches!(message, PlayerSessionMessage::Pause {})
    });
    call_browser(bus, MPRIS_PLAYER, "Play");
    read_player_messages_until(&mut socket, "the play command from the bus", |message| {
        matches!(message, PlayerSessionMessage::Play {})
    });
}

#[test]
fn quit_from_the_bus_closes_the_window_like_the_viewer_with_a_farewell() {
    let Some(process) = SessionProcessUnderTest::start("mpris-quit", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    call_browser(bus, MPRIS_ROOT, "Quit");

    let (_last_clock, _farewell, code) = read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    bus.wait_for_log_line("Browser window process exits");
    bus.wait_for_log_line("Browser session process finished");
}

#[test]
fn the_window_title_is_title_and_domain_and_the_log_never_shows_the_page() {
    let Some(process) = SessionProcessUnderTest::start("page-title", VideoMode::Window) else {
        return;
    };
    let display = process.display().expect("window mode has a display");
    let bus = &process.bus;
    bus.forbid_in_log(&[PAGE_TITLE, PAGE_DOMAIN]);
    let mut socket = open_attached_session_with(bus, &open_session_of_the_page());
    bus.wait_for_log_line("Browser window ready");
    send_all(&mut socket, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut socket, 1_000_000);

    // Закрывается только окно с ТОЧНО таким заголовком: закрытие и есть доказательство.
    display.close_fastiplayer_window_titled(EXPECTED_CAPTION);

    let (_last_clock, _farewell, code) = read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    // Журнал до самого конца процесса: ни заголовка, ни домена (`forbid_in_log`).
    bus.wait_for_log_line("Browser session process finished");
}
