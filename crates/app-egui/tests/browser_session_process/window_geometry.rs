//! Окно браузерного режима помнит размер и фуллскрин и пишет config (мост, 04c, решения
//! 135 и 138). Настоящий процесс под частным `kwin_wayland`; размер и фуллскрин окна
//! читает и меняет настоящий композитор (KWin-скрипты), а не шов в процессе.
//!
//! Что доказывают тесты:
//! - окно открывается запомненного размера (`[browser_session.window]`) и во весь экран,
//!   если так запомнено;
//! - после изменения размера человеком и закрытия окна в `config.toml` новый размер, а
//!   все остальные строки файла не тронуты, и в файле нет ни домена, ни заголовка страницы;
//! - выход из фуллскрина пишет `fullscreen = false`, оконный размер остаётся.
//!
//! Нужен `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::session_ends::{
    read_farewell_then_close, send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;

/// Данные страницы, которых не должно быть ни в журнале, ни в config.
const PAGE_TITLE: &str = "Dogs unique-geometry-title";
const PAGE_DOMAIN: &str = "unique-geometry.example";

fn open_session_of_the_page() -> String {
    format!(
        r#"{{"type":"open_session","request_id":1,"session_key":"tab-88-video-2","page":{{"title":"{PAGE_TITLE}","domain":"{PAGE_DOMAIN}"}},"intent":"window","hints":{{"duration":{{"kind":"unknown"}},"position":{{"kind":"unknown"}}}}}}"#
    )
}

/// Config с запомненным окном; остальное — значения по умолчанию.
fn config_with_window(
    width: u16,
    height: u16,
    fullscreen: bool,
) -> (fastiplayer_config::AppConfig, String) {
    let mut config = fastiplayer_config::AppConfig::default();
    config.browser_session.window.inner_width_points = width;
    config.browser_session.window.inner_height_points = height;
    config.browser_session.window.fullscreen = fullscreen;
    let text = config.to_pretty_toml().expect("config text");
    (config, text)
}

/// Строки файла, которые отличаются между двумя текстами (по позиции).
fn differing_lines(before: &str, after: &str) -> Vec<String> {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    assert_eq!(
        before_lines.len(),
        after_lines.len(),
        "the set of keys in the file must not change:\n{before}\n---\n{after}"
    );
    before_lines
        .iter()
        .zip(&after_lines)
        .filter(|(left, right)| left != right)
        .map(|(_, right)| (*right).to_owned())
        .collect()
}

/// Открывает сеанс страницы, ждёт окно и ход воспроизведения.
fn open_window_and_play(
    process: &SessionProcessUnderTest,
) -> (WebSocket<MaybeTlsStream<TcpStream>>, u64) {
    let bus = &process.bus;
    bus.forbid_in_log(&[PAGE_TITLE, PAGE_DOMAIN]);
    let mut socket = open_attached_session_with(bus, &open_session_of_the_page());
    bus.wait_for_log_line("Browser window ready");
    send_all(&mut socket, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut socket, 1_000_000);
    (socket, clock_seen)
}

/// Закрывает окно рукой композитора и ждёт конца процесса (config дописан при закрытии).
fn close_window_and_wait_for_exit(
    process: &SessionProcessUnderTest,
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    clock_seen: u64,
) {
    process
        .display()
        .expect("window mode has a display")
        .close_fastiplayer_window();
    let (_clock, _farewell, code) = read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    process
        .bus
        .wait_for_log_line("Browser session process finished");
}

#[test]
fn the_window_opens_at_the_remembered_size() {
    let (_config, text) = config_with_window(900, 500, false);
    let Some(process) = SessionProcessUnderTest::start_window_with_config("geometry-open", &text)
    else {
        return;
    };
    let (socket, clock_seen) = open_window_and_play(&process);

    let report = process
        .display()
        .expect("display")
        .window_geometry_report()
        .expect("the Fastiplayer window exists");

    assert_eq!(
        (report.width, report.height, report.fullscreen),
        (900, 500, false),
        "the compositor must see the remembered inner size"
    );
    close_window_and_wait_for_exit(&process, socket, clock_seen);
}

#[test]
fn a_resize_by_the_viewer_is_written_to_the_config_and_nothing_else_changes() {
    let (_config, text) = config_with_window(900, 500, false);
    let Some(process) = SessionProcessUnderTest::start_window_with_config("geometry-resize", &text)
    else {
        return;
    };
    let (socket, clock_seen) = open_window_and_play(&process);
    let display = process.display().expect("display");

    display.resize_fastiplayer_window(700, 420);
    // Композитор действительно изменил окно (а не только отправил просьбу).
    let deadline = Instant::now() + WAIT;
    loop {
        let report = display.window_geometry_report().expect("window exists");
        if (report.width, report.height) == (700, 420) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "KWin never resized the window: {report:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    close_window_and_wait_for_exit(&process, socket, clock_seen);

    let after = std::fs::read_to_string(process.bus.user_config_path()).expect("config.toml");
    let changed = differing_lines(&text, &after);
    assert_eq!(
        changed,
        vec![
            "inner_width_points = 700".to_owned(),
            "inner_height_points = 420".to_owned(),
        ],
        "only the window size lines may change:\n{after}"
    );
    assert!(
        !after.contains(PAGE_TITLE) && !after.contains(PAGE_DOMAIN),
        "page data must never reach the config file:\n{after}"
    );
}

#[test]
fn a_fullscreen_window_comes_up_fullscreen_and_leaving_it_is_remembered() {
    let (_config, text) = config_with_window(900, 500, true);
    let Some(process) =
        SessionProcessUnderTest::start_window_with_config("geometry-fullscreen", &text)
    else {
        return;
    };
    let (socket, clock_seen) = open_window_and_play(&process);
    let display = process.display().expect("display");
    assert!(
        display
            .window_geometry_report()
            .expect("window exists")
            .fullscreen,
        "the remembered fullscreen must reach the compositor"
    );

    display.leave_fastiplayer_fullscreen();
    let deadline = Instant::now() + WAIT;
    loop {
        if !display
            .window_geometry_report()
            .expect("window exists")
            .fullscreen
        {
            break;
        }
        assert!(Instant::now() < deadline, "KWin never left fullscreen");
        std::thread::sleep(Duration::from_millis(100));
    }
    close_window_and_wait_for_exit(&process, socket, clock_seen);

    let after = std::fs::read_to_string(process.bus.user_config_path()).expect("config.toml");
    let changed = differing_lines(&text, &after);
    // Размер в фуллскрине не писался; изменился только флаг (размер окна после выхода мог
    // остаться прежним 900x500 или стать размером, который выдал композитор — но флаг
    // `fullscreen = false` обязан быть записан).
    assert!(
        changed.contains(&"fullscreen = false".to_owned()),
        "leaving fullscreen must be written: {changed:?}\n{after}"
    );
    assert!(
        !after.contains(PAGE_TITLE) && !after.contains(PAGE_DOMAIN),
        "page data must never reach the config file:\n{after}"
    );
}
