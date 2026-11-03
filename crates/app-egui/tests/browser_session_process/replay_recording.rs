//! Сессия 09b2: «чёрный ящик» плеера и повтор его записи в настоящем процессе.
//!
//! Запись включается отладочной переменной окружения процесса (имя повторяет
//! `debug_recording::DEBUG_RECORD_VARIABLE` бинаря; тест не может импортировать
//! константу из бинарного crate-а). Повтор читает каталог записи в формате фикстур
//! протокола (`stream.json` + `NNN.json`/`NNN.bin`, шаги с необязательным `at_ms`) и
//! проигрывает его в `fastiplayer --browser-session` как экстеншен: с исходными паузами
//! или «быстро».
//!
//! Тесты без `#[ignore]` проверяют цепочку целиком: процесс A записывает сыгранный поток,
//! процесс B проигрывает именно эту запись и показывает все кадры. Игнорируемый тест
//! берёт настоящую запись с сайта из `FASTIPLAYER_REPLAY_RECORDING`:
//! ```text
//! FASTIPLAYER_REPLAY_RECORDING=/путь/к/session-1 \
//!   cargo test -p app-egui --test browser_session_process -- --ignored --nocapture replay_real_recording
//! ```
//! Необязательно: `FASTIPLAYER_REPLAY_FAST=1` — без пауз; `FASTIPLAYER_REPLAY_TAIL_MS` —
//! сколько держать сеанс после последнего шага (по умолчанию 3000).
//! `FASTIPLAYER_REPLAY_WINDOW=1` — окно на частном виртуальном дисплее (настоящий путь показа:
//! аппаратный декодер и рендер), `FASTIPLAYER_REPLAY_CONFIG=<config.toml>` — настройки показа
//! живого плеера. Без них кадры считает безоконный путь с программным декодером.

use super::*;

/// Имя отладочной переменной записи (см. заголовок файла).
const DEBUG_RECORD_VARIABLE: &str = "FASTIPLAYER_DEBUG_RECORD_BROWSER_STREAM";
/// Строка журнала, по которой видно «запись включена».
const RECORDING_ON_MARKER: &str = "Debug recording is ON";
/// Предупреждение об остановке записи (ровно одно на процесс).
const RECORDING_STOPPED_MARKER: &str = "Debug stream recording stopped";
/// Запись, которую играют тесты (та же, что у сквозного теста «играет до конца»).
const FIXTURE_RECORDING: &str = "two-source-buffers-mid-box-cuts";

/// Темп повтора.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pacing {
    /// С паузами между шагами, как они шли у страницы (`at_ms`).
    Original,
    /// Без пауз.
    Fast,
}

/// Шаг записи со временем прихода.
struct TimedStep {
    at: Duration,
    step: StreamStep,
}

/// Читает каталог записи; нет `at_ms` (фикстура) — шаг идёт без паузы.
fn load_recording(directory: &Path) -> Vec<TimedStep> {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join("stream.json")).expect("stream.json of the recording"),
    )
    .expect("stream.json is JSON");
    manifest["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|step| {
            let at = Duration::from_millis(step["at_ms"].as_u64().unwrap_or(0));
            let step = if let Some(position_us) = step["await_clock_us"].as_u64() {
                StreamStep::AwaitClock(position_us)
            } else if let Some(file) = step["text"].as_str() {
                // Текст — как записан, без обрезки: байт в байт.
                let text = std::fs::read_to_string(directory.join(file)).expect("text step");
                StreamStep::Text(text)
            } else {
                let file = step["binary"].as_str().expect("step is text or binary");
                StreamStep::Binary(std::fs::read(directory.join(file)).expect("binary step"))
            };
            TimedStep { at, step }
        })
        .collect()
}

/// Ждёт `period`, не копя непрочитанное: плеер шлёт часы, а повтор долгий.
fn wait_draining(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    period: Duration,
    received: &mut Vec<PlayerSessionMessage>,
) {
    let deadline = Instant::now() + period;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        set_read_timeout(socket, left.max(Duration::from_millis(1)));
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(message) = decode_message::<PlayerSessionMessage>(text.as_bytes()) {
                    received.push(message);
                }
            }
            Ok(_) => {}
            // Время вышло: сокет цел, читать можно дальше.
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("the session ended while the recording was paused: {error}"),
        }
    }
    set_read_timeout(socket, WAIT);
}

fn set_read_timeout(socket: &WebSocket<MaybeTlsStream<TcpStream>>, timeout: Duration) {
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(timeout))
            .expect("read timeout is set");
    }
}

/// Чем кончился повтор.
struct ReplayOutcome {
    /// Закончила ли сеанс сама запись (в ней был `tab_gone`).
    closed_by_recording: bool,
}

/// Проигрывает запись на провод. Возвращает, закрыла ли сеанс сама запись.
fn replay_steps(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    steps: Vec<TimedStep>,
    pacing: Pacing,
) -> ReplayOutcome {
    let started = Instant::now();
    let mut received = Vec::new();
    for TimedStep { at, step } in steps {
        if pacing == Pacing::Original {
            wait_draining(socket, at.saturating_sub(started.elapsed()), &mut received);
        }
        match step {
            StreamStep::Text(text) => {
                let is_tab_gone = text.contains(r#""type":"tab_gone""#);
                socket.send(Message::text(text)).expect("step is sent");
                if is_tab_gone {
                    assert_eq!(
                        read_close_code(socket),
                        CloseCode::from(SessionCloseReason::TabGone.close_code())
                    );
                    return ReplayOutcome {
                        closed_by_recording: true,
                    };
                }
            }
            StreamStep::Binary(bytes) => socket.send(Message::binary(bytes)).expect("step is sent"),
            StreamStep::AwaitClock(position_us) => {
                read_player_messages_until(socket, "the clock the recording waited for", |m| {
                    matches!(m, PlayerSessionMessage::Clock { position_us: clock, state: ClockState::Playing, .. }
                        if clock.as_micros() >= position_us)
                });
            }
        }
    }
    ReplayOutcome {
        closed_by_recording: false,
    }
}

/// Читает журнал процесса до строки `needle` и возвращает ВСЕ прочитанные строки.
fn log_lines_until(bus: &PrivateBusWithBrowserSession, needle: &str) -> Vec<String> {
    let deadline = Instant::now() + WAIT;
    let mut lines = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = bus
            .log_lines
            .recv_timeout(left)
            .unwrap_or_else(|error| panic!("no log line with {needle:?}: {error}"));
        bus.assert_line_is_allowed(&line);
        let reached = line.contains(needle);
        lines.push(line);
        if reached {
            return lines;
        }
    }
}

/// Играет запись в новом процессе до конца и возвращает строки журнала с итогом.
///
/// `tail` — сколько держать сеанс после последнего шага, если сама запись его не закрыла.
/// Сколько ждать выхода процесса с окном после конца повтора (на паузе он не выходит).
const WINDOW_LOG_GRACE: Duration = Duration::from_secs(12);

/// Как поднять процесс для повтора: без окна (тесты CI) или как у живого плеера —
/// окно на частном виртуальном дисплее и `config.toml` разработчика (рендер, VA-API).
struct ReplayProcess<'setup> {
    video: &'setup TestVideo,
    config_toml: Option<&'setup str>,
}

impl ReplayProcess<'static> {
    /// Без окна и без config: процесс CI.
    const HEADLESS: Self = Self {
        video: &TestVideo::Headless,
        config_toml: None,
    };
}

fn replay_in_fresh_process(
    name: &str,
    recording: &Path,
    pacing: Pacing,
    tail: Duration,
    process: &ReplayProcess<'_>,
) -> Vec<String> {
    let bus =
        PrivateBusWithBrowserSession::start_with_env(name, process.config_toml, process.video, &[]);
    let mut socket = open_attached_session(&bus);
    let outcome = replay_steps(&mut socket, load_recording(recording), pacing);
    if !outcome.closed_by_recording {
        let mut received = Vec::new();
        wait_draining(&mut socket, tail, &mut received);
        send_tab_gone_and_expect_close(&mut socket);
    }
    match process.video {
        TestVideo::Headless => log_lines_until(&bus, "Browser session process finished"),
        // Окно на паузе ждёт человека: берём журнал за короткий срок после конца.
        TestVideo::Window(_) => {
            log_lines_until_or_deadline(&bus, "Browser session process finished", WINDOW_LOG_GRACE)
        }
    }
}

/// Журнал процесса до строки `needle` или до срока (что раньше). Для ручного повтора с
/// окном: на паузе после ухода вкладки окно ждёт человека (решение 130) и процесс не
/// выходит сам, а сводки за время повтора нужны всё равно.
fn log_lines_until_or_deadline(
    bus: &PrivateBusWithBrowserSession,
    needle: &str,
    wait: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while let Ok(line) = bus
        .log_lines
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        let reached = line.contains(needle);
        lines.push(line);
        if reached {
            break;
        }
    }
    lines
}

/// Итоговая сводка плеера из строк журнала.
fn final_summary(lines: &[String]) -> &String {
    lines
        .iter()
        .find(|line| line.contains("Browser playback summary kind=final"))
        .unwrap_or_else(|| panic!("no final playback summary in {lines:#?}"))
}

/// Играет фикстуру в процессе, который пишет «чёрный ящик» в `record_root`.
/// Возвращает строки журнала процесса до его выхода.
fn play_fixture_while_recording(name: &str, record_root: &Path) -> Vec<String> {
    let bus = PrivateBusWithBrowserSession::start_with_env(
        name,
        None,
        &TestVideo::Headless,
        &[(
            DEBUG_RECORD_VARIABLE,
            record_root.to_str().expect("utf-8 path"),
        )],
    );
    let mut socket = open_attached_session(&bus);
    let (steps, _) = recorded_stream(FIXTURE_RECORDING);
    for step in steps {
        match step {
            StreamStep::Text(text) => socket.send(Message::text(text)).expect("sent"),
            StreamStep::Binary(bytes) => socket.send(Message::binary(bytes)).expect("sent"),
            StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
        }
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
    log_lines_until(&bus, "Browser session process finished")
}

/// Числа `at_ms` из `stream.json` записи по порядку.
fn recorded_times_ms(directory: &Path) -> Vec<u64> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("stream.json")).expect("manifest"))
            .expect("manifest JSON");
    manifest["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| {
            step["at_ms"]
                .as_u64()
                .expect("every recorded step has at_ms")
        })
        .collect()
}

#[test]
fn recorded_playthrough_equals_the_input_and_replays_to_the_renderer() {
    let record_root = tempfile::tempdir().expect("record root");
    let lines = play_fixture_while_recording("record-a", record_root.path());
    // Сам игровой прогон с записью не пострадал: все кадры показаны.
    assert_eq!(
        log_number(final_summary(&lines), "presented_frames"),
        FIXTURE_VIDEO_FRAMES
    );

    // Запись = вход байт в байт: файлы шагов равны присланному, плюс финальный `tab_gone`.
    let session_directory = record_root.path().join("session-1");
    let (sent_steps, _) = recorded_stream(FIXTURE_RECORDING);
    let recorded = load_recording(&session_directory);
    assert_eq!(recorded.len(), sent_steps.len() + 1, "steps + tab_gone");
    for (index, (sent, recorded)) in sent_steps.iter().zip(&recorded).enumerate() {
        match (sent, &recorded.step) {
            (StreamStep::Text(sent), StreamStep::Text(got)) => {
                assert_eq!(sent, got, "step {index}")
            }
            (StreamStep::Binary(sent), StreamStep::Binary(got)) => {
                assert_eq!(sent, got, "step {index}");
            }
            _ => panic!("step {index} changed its kind"),
        }
    }
    assert!(matches!(
        &recorded.last().expect("tab_gone").step,
        StreamStep::Text(text) if text.contains("tab_gone")
    ));
    // Время прихода не убывает.
    let times = recorded_times_ms(&session_directory);
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    // Что плеер отправил странице: часы дошли до `ended`.
    let outgoing =
        std::fs::read_to_string(session_directory.join("outgoing.jsonl")).expect("outgoing.jsonl");
    assert!(outgoing.contains(r#""state":"ended""#), "{outgoing}");
    assert!(
        outgoing
            .lines()
            .all(|line| line.starts_with(r#"{"at_ms":"#))
    );

    // Повтор именно этой записи в другом процессе (без записи) доходит до кадров.
    let replay_lines = replay_in_fresh_process(
        "record-b",
        &session_directory,
        Pacing::Fast,
        Duration::from_secs(1),
        &ReplayProcess::HEADLESS,
    );
    let summary = final_summary(&replay_lines);
    assert_eq!(
        log_number(summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "{summary}"
    );
    assert_eq!(log_number(summary, "position_regressions"), 0, "{summary}");
    // Без переменной процесс повтора ничего не пишет и не объявляет запись включённой.
    assert!(
        replay_lines
            .iter()
            .all(|line| !line.contains(RECORDING_ON_MARKER)),
        "{replay_lines:#?}"
    );
}

#[test]
fn original_pacing_keeps_the_recorded_pauses() {
    // Синтетическая запись: фикстура с паузой в 1,5 с перед последним шагом.
    let directory = tempfile::tempdir().expect("recording dir");
    let (steps, _) = recorded_stream(FIXTURE_RECORDING);
    let mut manifest_steps = Vec::new();
    let last = steps.len() - 1;
    for (index, step) in steps.iter().enumerate() {
        let at_ms = if index == last { 1_500 } else { 0 };
        let (key, file) = match step {
            StreamStep::Text(text) => {
                let file = format!("{:03}.json", index + 1);
                std::fs::write(directory.path().join(&file), text).expect("text");
                ("text", file)
            }
            StreamStep::Binary(bytes) => {
                let file = format!("{:03}.bin", index + 1);
                std::fs::write(directory.path().join(&file), bytes).expect("bytes");
                ("binary", file)
            }
            StreamStep::AwaitClock(_) => unreachable!("no pauses in the fixture"),
        };
        manifest_steps.push(format!(r#"{{"{key}":"{file}","at_ms":{at_ms}}}"#));
    }
    std::fs::write(
        directory.path().join("stream.json"),
        format!(r#"{{"steps":[{}]}}"#, manifest_steps.join(",")),
    )
    .expect("manifest");

    let started = Instant::now();
    let lines = replay_in_fresh_process(
        "record-paced",
        directory.path(),
        Pacing::Original,
        Duration::from_secs(1),
        &ReplayProcess::HEADLESS,
    );
    // Пауза записи соблюдена: повтор не короче неё, и кадры всё равно показаны.
    assert!(started.elapsed() >= Duration::from_millis(1_500));
    assert_eq!(
        log_number(final_summary(&lines), "presented_frames"),
        FIXTURE_VIDEO_FRAMES
    );
}

#[test]
fn unwritable_record_directory_keeps_playing_with_one_warning() {
    use std::os::unix::fs::PermissionsExt;
    let record_root = tempfile::tempdir().expect("record root");
    std::fs::set_permissions(record_root.path(), std::fs::Permissions::from_mode(0o500))
        .expect("read-only root");
    // От root права не действуют: тогда запись удалась бы, и проверять нечего.
    if std::fs::create_dir(record_root.path().join("probe")).is_ok() {
        return;
    }
    let lines = play_fixture_while_recording("record-readonly", record_root.path());
    std::fs::set_permissions(record_root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("restore permissions");

    let warnings = lines
        .iter()
        .filter(|line| line.contains(RECORDING_STOPPED_MARKER))
        .count();
    assert_eq!(warnings, 1, "{lines:#?}");
    // Плеер доиграл: ошибка записи на показ не повлияла.
    assert_eq!(
        log_number(final_summary(&lines), "presented_frames"),
        FIXTURE_VIDEO_FRAMES
    );
    assert_eq!(
        std::fs::read_dir(record_root.path()).expect("root").count(),
        0
    );
}

/// Ручной повтор настоящей записи сайта в настоящем процессе (см. заголовок файла).
/// Печатает сводки плеера: `presented_frames`, `position_ms`, `state`.
#[test]
#[ignore = "needs a real recording: FASTIPLAYER_REPLAY_RECORDING=<session directory>"]
fn replay_real_recording_in_the_real_process() {
    let directory = PathBuf::from(
        std::env::var_os("FASTIPLAYER_REPLAY_RECORDING")
            .expect("set FASTIPLAYER_REPLAY_RECORDING to a recording directory"),
    );
    let pacing = if std::env::var_os("FASTIPLAYER_REPLAY_FAST").is_some() {
        Pacing::Fast
    } else {
        Pacing::Original
    };
    let tail_ms = std::env::var("FASTIPLAYER_REPLAY_TAIL_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000);
    // `FASTIPLAYER_REPLAY_WINDOW=1` — как у живого плеера: окно на частном виртуальном
    // дисплее и настоящий путь показа (VA-API, GLES2 из config). Безоконный путь считает
    // кадры программным декодером и прячет сбои аппаратного (находка 09b2).
    let window_display = std::env::var_os("FASTIPLAYER_REPLAY_WINDOW").map(|_| {
        window::VirtualWaylandDisplay::start("replay-real")
            .expect("FASTIPLAYER_REPLAY_WINDOW needs kwin_wayland for a private display")
    });
    let video = window_display.map_or(TestVideo::Headless, TestVideo::Window);
    // `FASTIPLAYER_REPLAY_CONFIG=<config.toml>` — настройки показа живого плеера.
    let config_toml = std::env::var_os("FASTIPLAYER_REPLAY_CONFIG").map(|path| {
        std::fs::read_to_string(&path)
            .expect("FASTIPLAYER_REPLAY_CONFIG must name a readable config.toml")
    });
    let process = ReplayProcess {
        video: &video,
        config_toml: config_toml.as_deref(),
    };
    let lines = replay_in_fresh_process(
        "replay-real",
        &directory,
        pacing,
        Duration::from_millis(tail_ms),
        &process,
    );
    // Сводки и отказы плеера (их текст — коды ошибок и времена, без данных страницы):
    // по ним видно, пошёл ли плеер за страницей (находка 09b2: `SeekTargetExpired`).
    for line in lines.iter().filter(|line| {
        line.contains("Browser playback summary") || line.contains("Player worker command failed")
    }) {
        println!("{line}");
    }
    // Последняя сводка (с окном процесс на паузе может не выйти — итоговой тогда нет).
    let summary = lines
        .iter()
        .rev()
        .find(|line| line.contains("Browser playback summary"))
        .expect("the player printed no playback summary");
    // Сбои декодера — главный след находки 09b2 (аппаратный H.264 на Twitch).
    let decode_errors = lines
        .iter()
        .filter(|line| line.contains("Decode error"))
        .count();
    println!(
        "FINAL presented_frames={} decode_errors={decode_errors} {summary}",
        log_number(summary, "presented_frames")
    );
}
