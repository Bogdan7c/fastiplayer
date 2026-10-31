//! Сквозной тест инварианта 7: после сеанса в HOME и XDG нет следов просмотра
//! (мост, сессия 04c, пункт 6).
//!
//! Настоящий процесс под частной шиной играет ролик до показа кадров, зритель делает то,
//! что процесс запоминает (в окне — меняет размер окна рукой композитора), сеанс
//! кончается, процесс выходит. ДО удаления временного корня сканируется всё дерево
//! `xdg/**` и `home/**`:
//! - разрешены только `xdg/config/fastiplayer/config.toml` и его замок записи
//!   `config.toml.lock`; любой другой файл (кэш, состояние, история, резюме) — провал со
//!   списком путей;
//! - в каждом разрешённом файле нет домена, заголовка страницы и ключа сеанса;
//! - `config.toml` разбирается строгим разбором текущей схемы, и вне секции
//!   `[browser_session]` он равен тому, что был до сеанса (плейлист, resume и прочие
//!   настройки процесс не трогает).
//!
//! Без окна процесс не коммитит ничего: файлов нет вовсе (если config не клали заранее).
//! Окно с чистым HOME создаёт config с геометрией окна и больше ничего.
//!
//! Кэш шейдеров драйвера (Mesa) процесс в оконном режиме мог бы положить в
//! `xdg/cache` — это не наш след, поэтому он выключен `MESA_SHADER_CACHE_DISABLE`.
//!
//! Окно требует `kwin_wayland` (иначе SKIPPED).

use std::collections::BTreeSet;

use super::session_ends::{
    read_farewell_then_close, send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;

/// Данные страницы, которых не должно быть ни в одном файле.
const PAGE_TITLE: &str = "Birds unique-home-title-q7";
const PAGE_DOMAIN: &str = "unique-home.example";
const SESSION_KEY: &str = "tab-91-video-5";

/// Окружение процесса: драйвер не пишет свой кэш в `xdg/cache`.
const NO_DRIVER_CACHE: &[(&str, &str)] = &[("MESA_SHADER_CACHE_DISABLE", "true")];

fn open_session_of_the_page() -> String {
    format!(
        r#"{{"type":"open_session","request_id":1,"session_key":"{SESSION_KEY}","page":{{"title":"{PAGE_TITLE}","domain":"{PAGE_DOMAIN}"}},"intent":"window","hints":{{"duration":{{"kind":"unknown"}},"position":{{"kind":"unknown"}}}}}}"#
    )
}

/// Что разрешено найти в config-каталоге после сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedConfig {
    /// Файлов нет вообще: процесс ничего не коммитит (config заранее не клали).
    NoFiles,
    /// Есть `config.toml` и/или замок `config.toml.lock`, и больше ничего.
    ConfigAndLockOnly,
}

/// Все файлы под `root` (каталоги не считаются), пути относительно `root`.
fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(directory: &Path, root: &Path, found: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("directory entry").path();
            // Символьная ссылка тоже след: считаем её файлом, не заходя внутрь.
            let is_directory = std::fs::symlink_metadata(&path)
                .expect("metadata")
                .file_type()
                .is_dir();
            if is_directory {
                walk(&path, root, found);
            } else {
                found.insert(path.strip_prefix(root).expect("under root").to_path_buf());
            }
        }
    }
    let mut found = BTreeSet::new();
    walk(root, root, &mut found);
    found
}

/// Сканирует `xdg/**` и `home/**` процесса и проверяет всё, что описано в шапке модуля.
/// `seeded_config` — текст config, положенный до старта (`None` — HOME был чистым).
fn assert_home_stays_clean(
    bus: &PrivateBusWithBrowserSession,
    seeded_config: Option<&str>,
    expected: ExpectedConfig,
) {
    let found: BTreeSet<PathBuf> = ["xdg", "home"]
        .iter()
        .flat_map(|top| {
            let root = bus.directory.join(top);
            files_under(&root)
                .into_iter()
                .map(move |relative| Path::new(top).join(relative))
                .collect::<Vec<_>>()
        })
        .collect();

    let config_directory = Path::new("xdg/config/fastiplayer");
    let config_file = config_directory.join("config.toml");
    let lock_file = config_directory.join("config.toml.lock");
    let allowed: BTreeSet<PathBuf> = match expected {
        ExpectedConfig::NoFiles => BTreeSet::new(),
        ExpectedConfig::ConfigAndLockOnly => [config_file.clone(), lock_file.clone()].into(),
    };
    let unexpected: Vec<&PathBuf> = found.difference(&allowed).collect();
    assert!(
        unexpected.is_empty(),
        "the session left traces of viewing in HOME/XDG: {unexpected:?} (all files: {found:?})"
    );

    // Ни в одном найденном файле нет данных страницы и ключа сеанса.
    for relative in &found {
        let text = String::from_utf8_lossy(&std::fs::read(bus.directory.join(relative)).unwrap())
            .into_owned();
        for secret in [PAGE_TITLE, PAGE_DOMAIN, SESSION_KEY] {
            assert!(
                !text.contains(secret),
                "{} contains {secret:?}:\n{text}",
                relative.display()
            );
        }
    }

    // Config: строгий разбор схемы проходит, вне `[browser_session]` ничего не изменилось.
    if found.contains(&config_file) {
        let loaded = fastiplayer_config::load_from_path(bus.user_config_path())
            .expect("config.toml written by the process parses with the current schema");
        let mut baseline = match seeded_config {
            Some(text) => {
                let seeded_path = bus.directory.join("seeded-config.toml");
                std::fs::write(&seeded_path, text).expect("seeded copy");
                fastiplayer_config::load_from_path(&seeded_path)
                    .expect("seeded config parses")
                    .config
            }
            None => fastiplayer_config::AppConfig::default(),
        };
        // Секция браузерного окна — единственное, что процесс вправе менять.
        baseline.browser_session = loaded.config.browser_session;
        assert_eq!(
            loaded.config, baseline,
            "the process changed config outside [browser_session]"
        );
    }
}

/// Тест доказывает «ролик играл до кадров» строкой итога с показанными кадрами.
fn assert_frames_were_presented(summary: &str) {
    assert!(
        log_number(summary, "presented_frames") > 0,
        "nothing reached the presentation: {summary}"
    );
}

/// Сеанс без окна: полный ролик, затем `tab_gone`.
fn headless_session_leaves_nothing(name: &str, seeded_config: Option<&str>) {
    let bus = PrivateBusWithBrowserSession::start_with_env(
        &format!("{name}-headless"),
        seeded_config,
        &TestVideo::Headless,
        NO_DRIVER_CACHE,
    );
    bus.forbid_in_log(&[PAGE_TITLE, PAGE_DOMAIN]);
    let mut socket = open_attached_session_with(&bus, &open_session_of_the_page());
    let (steps, _bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
            StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
        };
        socket.send(message).expect("stream step is sent");
    }
    read_player_messages_until(&mut socket, "the clock state ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES
    );
    bus.wait_for_log_line("Browser session process finished");

    // Без окна нечего запоминать: при чистом HOME файлов нет вовсе.
    let expected = if seeded_config.is_some() {
        ExpectedConfig::ConfigAndLockOnly
    } else {
        ExpectedConfig::NoFiles
    };
    assert_home_stays_clean(&bus, seeded_config, expected);
}

/// Сеанс в окне: ролик играет, зритель меняет размер окна, затем закрывает окно.
fn window_session_leaves_only_geometry(name: &str, seeded_config: Option<&str>) {
    let Some(display) = window::VirtualWaylandDisplay::start(name) else {
        return;
    };
    let video = TestVideo::Window(display);
    let bus = PrivateBusWithBrowserSession::start_with_env(
        &format!("{name}-window"),
        seeded_config,
        &video,
        NO_DRIVER_CACHE,
    );
    let TestVideo::Window(display) = &video else {
        unreachable!("the video was built as a window just above");
    };
    bus.forbid_in_log(&[PAGE_TITLE, PAGE_DOMAIN]);
    let mut socket = open_attached_session_with(&bus, &open_session_of_the_page());
    bus.wait_for_log_line("Browser window ready");
    send_all(&mut socket, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut socket, 1_000_000);

    // Запоминаемое: зритель тянет край окна (композитор подтверждает новый размер).
    display.resize_fastiplayer_window(700, 420);
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

    display.close_fastiplayer_window();
    let (_clock, _farewell, code) = read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_frames_were_presented(&summary);
    bus.wait_for_log_line("Browser session process finished");

    assert_home_stays_clean(&bus, seeded_config, ExpectedConfig::ConfigAndLockOnly);
    // Геометрия действительно запомнена (иначе тест «чисто» прошёл бы и на пустом config).
    let written = fastiplayer_config::load_from_path(bus.user_config_path())
        .expect("config.toml parses")
        .config;
    assert_eq!(
        (
            written.browser_session.window.inner_width_points,
            written.browser_session.window.inner_height_points
        ),
        (700, 420),
        "the resize must be the only thing remembered"
    );
}

/// Стартовый config: значения по умолчанию, но с запомненным окном (процесс его читает).
fn seeded_default_config() -> String {
    let mut config = fastiplayer_config::AppConfig::default();
    config.browser_session.window.inner_width_points = 900;
    config.browser_session.window.inner_height_points = 500;
    config.to_pretty_toml().expect("config text")
}

#[test]
fn headless_session_with_clean_home_leaves_no_files() {
    headless_session_leaves_nothing("home-clean", None);
}

#[test]
fn headless_session_with_existing_config_touches_only_that_config() {
    headless_session_leaves_nothing("home-seeded", Some(&seeded_default_config()));
}

#[test]
fn window_session_with_clean_home_leaves_only_window_geometry() {
    window_session_leaves_only_geometry("home-window-clean", None);
}

#[test]
fn window_session_with_existing_config_changes_only_browser_session() {
    window_session_leaves_only_geometry("home-window-seeded", Some(&seeded_default_config()));
}
